use crate::cdsl::operands::{Operand, OperandKind};

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::slice;

/// An immediate field in an instruction format.
///
/// This corresponds to a single member of a variant of the `InstructionData`
/// data type.
#[derive(Debug)]
pub struct FormatField {
    /// Immediate operand kind.
    pub kind: OperandKind,

    /// Member name in InstructionData variant.
    pub member: &'static str,
}

/// Every instruction opcode has a corresponding instruction format which determines the number of
/// operands and their kinds. Instruction formats are identified structurally, i.e., the format of
/// an instruction is derived from the kinds of operands used in its declaration.
///
/// The instruction format stores two separate lists of operands: Immediates and values. Immediate
/// operands (including entity references) are represented as explicit members in the
/// `InstructionData` variants. The value operands are stored differently, depending on how many
/// there are.  Beyond a certain point, instruction formats switch to an external value list for
/// storing value arguments. Value lists can hold an arbitrary number of values.
///
/// All instruction formats must be predefined in the meta shared/formats.rs module.
#[derive(Debug)]
pub struct InstructionFormat {
    /// Instruction format name in CamelCase. This is used as a Rust variant name in both the
    /// `InstructionData` and `InstructionFormat` enums.
    pub name: &'static str,

    pub num_value_operands: usize,

    pub has_value_list: bool,

    pub imm_fields: Vec<FormatField>,

    /// Index of the value input operand that is used to infer the controlling type variable. By
    /// default, this is `0`, the first `value` operand. The index is relative to the values only,
    /// ignoring immediate operands.
    pub typevar_operand: Option<usize>,
}

impl fmt::Display for InstructionFormat {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        let imm_args = self
            .imm_fields
            .iter()
            .map(|field| format!("{}: {}", field.member, field.kind.name))
            .collect::<Vec<_>>()
            .join(", ");
        fmt.write_fmt(format_args!(
            "{}(imms=({}), vals={})",
            self.name, imm_args, self.num_value_operands
        ))?;
        Ok(())
    }
}

impl InstructionFormat {
    pub fn imm_by_name(&self, name: &'static str) -> &FormatField {
        self.imm_fields
            .iter()
            .find(|&field| field.member == name)
            .unwrap_or_else(|| {
                panic!(
                    "unexpected immediate field named {} in instruction format {}",
                    name, self.name
                )
            })
    }
}

pub struct InstructionFormatBuilder {
    name: &'static str,
    num_value_operands: usize,
    has_value_list: bool,
    imm_fields: Vec<FormatField>,
    typevar_operand: Option<usize>,
}

impl InstructionFormatBuilder {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            num_value_operands: 0,
            has_value_list: false,
            imm_fields: Vec::new(),
            typevar_operand: None,
        }
    }

    pub fn value(mut self) -> Self {
        self.num_value_operands += 1;
        self
    }

    pub fn varargs(mut self) -> Self {
        self.has_value_list = true;
        self
    }

    pub fn imm(mut self, operand_kind: &OperandKind) -> Self {
        let field = FormatField {
            kind: operand_kind.clone(),
            member: operand_kind.default_member.unwrap(),
        };
        self.imm_fields.push(field);
        self
    }

    pub fn imm_with_name(mut self, member: &'static str, operand_kind: &OperandKind) -> Self {
        let field = FormatField {
            kind: operand_kind.clone(),
            member,
        };
        self.imm_fields.push(field);
        self
    }

    pub fn typevar_operand(mut self, operand_index: usize) -> Self {
        assert!(self.typevar_operand.is_none());
        assert!(self.has_value_list || operand_index < self.num_value_operands);
        self.typevar_operand = Some(operand_index);
        self
    }

    pub fn build(self) -> InstructionFormat {
        let typevar_operand = if self.typevar_operand.is_some() {
            self.typevar_operand
        } else if self.has_value_list || self.num_value_operands > 0 {
            // Default to the first value operand, if there's one.
            Some(0)
        } else {
            None
        };

        InstructionFormat {
            name: self.name,
            num_value_operands: self.num_value_operands,
            has_value_list: self.has_value_list,
            imm_fields: self.imm_fields,
            typevar_operand,
        }
    }
}

pub struct FormatRegistry {
    /// Map (immediate kinds names, number of values, has varargs) to an instruction format.
    sig_to_index: HashMap<(Vec<String>, usize, bool), usize>,
    formats: Vec<Rc<InstructionFormat>>,
    name_set: HashSet<&'static str>,
}

impl FormatRegistry {
    pub fn new() -> Self {
        Self {
            sig_to_index: HashMap::new(),
            formats: Vec::new(),
            name_set: HashSet::new(),
        }
    }

    /// Find an existing instruction format that matches the given lists of instruction inputs and
    /// outputs.
    pub fn lookup(&self, operands_in: &Vec<Operand>) -> &Rc<InstructionFormat> {
        let mut imm_keys = Vec::new();
        let mut num_values = 0;
        let mut has_varargs = false;

        for operand in operands_in.iter() {
            if operand.is_value() {
                num_values += 1;
            }
            if !has_varargs {
                has_varargs = operand.is_varargs();
            }
            if let Some(imm_key) = operand.kind.imm_key() {
                imm_keys.push(imm_key);
            }
        }

        let sig = (imm_keys, num_values, has_varargs);
        let index = *self
            .sig_to_index
            .get(&sig)
            .expect("unknown InstructionFormat; please define it in shared/formats.rs first");
        &self.formats[index]
    }

    pub fn by_name(&self, name: &str) -> &Rc<InstructionFormat> {
        &self
            .formats
            .iter()
            .find(|format| format.name == name)
            .unwrap_or_else(|| panic!("format with name '{}' doesn't exist", name))
    }

    pub fn insert(&mut self, inst_format: InstructionFormatBuilder) {
        let name = &inst_format.name;
        if !self.name_set.insert(name) {
            panic!(
                "Trying to add an InstructionFormat named {}, but it already exists!",
                name
            );
        }

        let format = inst_format.build();

        // Compute key.
        let imm_keys = format
            .imm_fields
            .iter()
            .map(|field| field.kind.imm_key().unwrap())
            .collect();
        let key = (imm_keys, format.num_value_operands, format.has_value_list);

        let index = self.formats.len();
        self.formats.push(Rc::new(format));
        if let Some(already_inserted) = self.sig_to_index.insert(key, index) {
            panic!(
                "duplicate InstructionFormat: trying to insert '{}' while '{}' already has the same structure.",
                self.formats[index].name,
                self.formats[already_inserted].name
            );
        }
    }

    pub fn iter(&self) -> slice::Iter<Rc<InstructionFormat>> {
        self.formats.iter()
    }
}
