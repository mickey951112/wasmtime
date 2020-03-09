use super::address_transform::AddressTransform;
use super::attr::{clone_die_attributes, FileAttributeContext};
use super::expression::compile_expression;
use super::line_program::clone_line_program;
use super::range_info_builder::RangeInfoBuilder;
use super::refs::{PendingDebugInfoRefs, PendingUnitRefs, UnitRefsMap};
use super::utils::{add_internal_types, append_vmctx_info, get_function_frame_info};
use super::{DebugInputContext, Reader, TransformError};
use anyhow::{Context, Error};
use gimli::write;
use gimli::{AttributeValue, DebuggingInformationEntry, Unit};
use std::collections::HashSet;
use wasmtime_environ::entity::EntityRef;
use wasmtime_environ::isa::TargetIsa;
use wasmtime_environ::{ModuleVmctxInfo, ValueLabelsRanges};

struct InheritedAttr<T> {
    stack: Vec<(usize, T)>,
}

impl<T> InheritedAttr<T> {
    fn new() -> Self {
        InheritedAttr { stack: Vec::new() }
    }

    fn update(&mut self, depth: usize) {
        while !self.stack.is_empty() && self.stack.last().unwrap().0 >= depth {
            self.stack.pop();
        }
    }

    fn push(&mut self, depth: usize, value: T) {
        self.stack.push((depth, value));
    }

    fn top(&self) -> Option<&T> {
        self.stack.last().map(|entry| &entry.1)
    }

    fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

fn get_base_type_name<R>(
    type_entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R, R::Offset>,
    context: &DebugInputContext<R>,
) -> Result<String, Error>
where
    R: Reader,
{
    // FIXME remove recursion.
    if let Some(AttributeValue::UnitRef(ref offset)) = type_entry.attr_value(gimli::DW_AT_type)? {
        let mut entries = unit.entries_at_offset(*offset)?;
        entries.next_entry()?;
        if let Some(die) = entries.current() {
            if let Some(AttributeValue::DebugStrRef(str_offset)) =
                die.attr_value(gimli::DW_AT_name)?
            {
                return Ok(String::from(
                    context.debug_str.get_str(str_offset)?.to_string()?,
                ));
            }
            match die.tag() {
                gimli::DW_TAG_const_type => {
                    return Ok(format!("const {}", get_base_type_name(die, unit, context)?));
                }
                gimli::DW_TAG_pointer_type => {
                    return Ok(format!("{}*", get_base_type_name(die, unit, context)?));
                }
                gimli::DW_TAG_reference_type => {
                    return Ok(format!("{}&", get_base_type_name(die, unit, context)?));
                }
                gimli::DW_TAG_array_type => {
                    return Ok(format!("{}[]", get_base_type_name(die, unit, context)?));
                }
                _ => (),
            }
        }
    }
    Ok(String::from("??"))
}

fn replace_pointer_type<R>(
    parent_id: write::UnitEntryId,
    comp_unit: &mut write::Unit,
    wp_die_id: write::UnitEntryId,
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R, R::Offset>,
    context: &DebugInputContext<R>,
    out_strings: &mut write::StringTable,
    pending_die_refs: &mut PendingUnitRefs,
) -> Result<write::UnitEntryId, Error>
where
    R: Reader,
{
    let die_id = comp_unit.add(parent_id, gimli::DW_TAG_structure_type);
    let die = comp_unit.get_mut(die_id);

    let name = format!(
        "WebAssemblyPtrWrapper<{}>",
        get_base_type_name(entry, unit, context)?
    );
    die.set(
        gimli::DW_AT_name,
        write::AttributeValue::StringRef(out_strings.add(name.as_str())),
    );
    die.set(gimli::DW_AT_byte_size, write::AttributeValue::Data1(4));

    let p_die_id = comp_unit.add(die_id, gimli::DW_TAG_template_type_parameter);
    let p_die = comp_unit.get_mut(p_die_id);
    p_die.set(
        gimli::DW_AT_name,
        write::AttributeValue::StringRef(out_strings.add("T")),
    );
    p_die.set(
        gimli::DW_AT_type,
        write::AttributeValue::ThisUnitEntryRef(wp_die_id),
    );
    if let Some(AttributeValue::UnitRef(ref offset)) = entry.attr_value(gimli::DW_AT_type)? {
        pending_die_refs.insert(p_die_id, gimli::DW_AT_type, *offset);
    }

    let m_die_id = comp_unit.add(die_id, gimli::DW_TAG_member);
    let m_die = comp_unit.get_mut(m_die_id);
    m_die.set(
        gimli::DW_AT_name,
        write::AttributeValue::StringRef(out_strings.add("__ptr")),
    );
    m_die.set(
        gimli::DW_AT_type,
        write::AttributeValue::ThisUnitEntryRef(wp_die_id),
    );
    m_die.set(
        gimli::DW_AT_data_member_location,
        write::AttributeValue::Data1(0),
    );
    Ok(die_id)
}

pub(crate) fn clone_unit<'a, R>(
    unit: Unit<R, R::Offset>,
    context: &DebugInputContext<R>,
    addr_tr: &'a AddressTransform,
    value_ranges: &'a ValueLabelsRanges,
    out_encoding: gimli::Encoding,
    module_info: &ModuleVmctxInfo,
    out_units: &mut write::UnitTable,
    out_strings: &mut write::StringTable,
    translated: &mut HashSet<u32>,
    isa: &dyn TargetIsa,
) -> Result<Option<(write::UnitId, UnitRefsMap, PendingDebugInfoRefs)>, Error>
where
    R: Reader,
{
    let mut die_ref_map = UnitRefsMap::new();
    let mut pending_die_refs = PendingUnitRefs::new();
    let mut pending_di_refs = PendingDebugInfoRefs::new();
    let mut stack = Vec::new();

    // Iterate over all of this compilation unit's entries.
    let mut entries = unit.entries();
    let (mut comp_unit, unit_id, file_map, cu_low_pc, wp_die_id, vmctx_die_id) =
        if let Some((depth_delta, entry)) = entries.next_dfs()? {
            assert_eq!(depth_delta, 0);
            let (out_line_program, debug_line_offset, file_map) = clone_line_program(
                &unit,
                entry,
                addr_tr,
                out_encoding,
                context.debug_str,
                context.debug_line,
                out_strings,
            )?;

            if entry.tag() == gimli::DW_TAG_compile_unit {
                let unit_id = out_units.add(write::Unit::new(out_encoding, out_line_program));
                let comp_unit = out_units.get_mut(unit_id);

                let root_id = comp_unit.root();
                die_ref_map.insert(entry.offset(), root_id);

                let cu_low_pc = if let Some(AttributeValue::Addr(addr)) =
                    entry.attr_value(gimli::DW_AT_low_pc)?
                {
                    addr
                } else {
                    // FIXME? return Err(TransformError("No low_pc for unit header").into());
                    0
                };

                clone_die_attributes(
                    entry,
                    context,
                    addr_tr,
                    None,
                    unit.encoding(),
                    comp_unit,
                    root_id,
                    None,
                    None,
                    cu_low_pc,
                    out_strings,
                    &mut pending_die_refs,
                    &mut pending_di_refs,
                    FileAttributeContext::Root(Some(debug_line_offset)),
                    isa,
                )?;

                let (wp_die_id, vmctx_die_id) =
                    add_internal_types(comp_unit, root_id, out_strings, module_info);

                stack.push(root_id);
                (
                    comp_unit,
                    unit_id,
                    file_map,
                    cu_low_pc,
                    wp_die_id,
                    vmctx_die_id,
                )
            } else {
                return Err(TransformError("Unexpected unit header").into());
            }
        } else {
            return Ok(None); // empty
        };
    let mut skip_at_depth = None;
    let mut current_frame_base = InheritedAttr::new();
    let mut current_value_range = InheritedAttr::new();
    let mut current_scope_ranges = InheritedAttr::new();
    while let Some((depth_delta, entry)) = entries.next_dfs()? {
        let depth_delta = if let Some((depth, cached)) = skip_at_depth {
            let new_depth = depth + depth_delta;
            if new_depth > 0 {
                skip_at_depth = Some((new_depth, cached));
                continue;
            }
            skip_at_depth = None;
            new_depth + cached
        } else {
            depth_delta
        };

        if !context
            .reachable
            .contains(&entry.offset().to_unit_section_offset(&unit))
        {
            // entry is not reachable: discarding all its info.
            skip_at_depth = Some((0, depth_delta));
            continue;
        }

        let new_stack_len = stack.len().wrapping_add(depth_delta as usize);
        current_frame_base.update(new_stack_len);
        current_scope_ranges.update(new_stack_len);
        current_value_range.update(new_stack_len);
        let range_builder = if entry.tag() == gimli::DW_TAG_subprogram {
            let range_builder = RangeInfoBuilder::from_subprogram_die(
                entry,
                context,
                unit.encoding(),
                addr_tr,
                cu_low_pc,
            )?;
            if let RangeInfoBuilder::Function(func_index) = range_builder {
                if let Some(frame_info) =
                    get_function_frame_info(module_info, func_index, value_ranges)
                {
                    current_value_range.push(new_stack_len, frame_info);
                }
                translated.insert(func_index.index() as u32);
                current_scope_ranges.push(new_stack_len, range_builder.get_ranges(addr_tr));
                Some(range_builder)
            } else {
                // FIXME current_scope_ranges.push()
                None
            }
        } else {
            let high_pc = entry.attr_value(gimli::DW_AT_high_pc)?;
            let ranges = entry.attr_value(gimli::DW_AT_ranges)?;
            if high_pc.is_some() || ranges.is_some() {
                let range_builder =
                    RangeInfoBuilder::from(entry, context, unit.encoding(), cu_low_pc)?;
                current_scope_ranges.push(new_stack_len, range_builder.get_ranges(addr_tr));
                Some(range_builder)
            } else {
                None
            }
        };

        if depth_delta <= 0 {
            for _ in depth_delta..1 {
                stack.pop();
            }
        } else {
            assert_eq!(depth_delta, 1);
        }

        if let Some(AttributeValue::Exprloc(expr)) = entry.attr_value(gimli::DW_AT_frame_base)? {
            if let Some(expr) = compile_expression(&expr, unit.encoding(), None, isa)? {
                current_frame_base.push(new_stack_len, expr);
            }
        }

        let parent = stack.last().unwrap();

        if entry.tag() == gimli::DW_TAG_pointer_type {
            // Wrap pointer types.
            // TODO reference types?
            let die_id = replace_pointer_type(
                *parent,
                comp_unit,
                wp_die_id,
                entry,
                &unit,
                context,
                out_strings,
                &mut pending_die_refs,
            )?;
            stack.push(die_id);
            assert_eq!(stack.len(), new_stack_len);
            die_ref_map.insert(entry.offset(), die_id);
            continue;
        }

        let die_id = comp_unit.add(*parent, entry.tag());

        stack.push(die_id);
        assert_eq!(stack.len(), new_stack_len);
        die_ref_map.insert(entry.offset(), die_id);

        clone_die_attributes(
            entry,
            context,
            addr_tr,
            current_value_range.top(),
            unit.encoding(),
            &mut comp_unit,
            die_id,
            range_builder,
            current_scope_ranges.top(),
            cu_low_pc,
            out_strings,
            &mut pending_die_refs,
            &mut pending_di_refs,
            FileAttributeContext::Children(&file_map, current_frame_base.top()),
            isa,
        )?;

        if entry.tag() == gimli::DW_TAG_subprogram && !current_scope_ranges.is_empty() {
            append_vmctx_info(
                comp_unit,
                die_id,
                vmctx_die_id,
                addr_tr,
                current_value_range.top(),
                current_scope_ranges.top().context("range")?,
                out_strings,
                isa,
            )?;
        }
    }
    die_ref_map.patch(pending_die_refs, comp_unit);
    Ok(Some((unit_id, die_ref_map, pending_di_refs)))
}
