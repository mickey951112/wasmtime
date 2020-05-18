use proptest::prelude::*;
use wiggle::{BorrowChecker, GuestMemory, GuestType};
use wiggle_test::{impl_errno, HostMemory, MemArea, WasiCtx};

wiggle::from_witx!({
    witx: ["tests/union.witx"],
    ctx: WasiCtx,
});

impl_errno!(types::Errno, types::GuestErrorConversion);

// Avoid panics on overflow
fn mult_lose_overflow(a: i32, b: u32) -> i32 {
    let a_64: i64 = a as i64;
    let b_64: i64 = b as i64;
    let product = a_64 * b_64;
    product as i32
}

// Avoid assert_eq(NaN, NaN) failures
fn mult_zero_nan(a: f32, b: u32) -> f32 {
    if a.is_nan() {
        0.0
    } else {
        let product = a * b as f32;
        if product.is_nan() {
            0.0
        } else {
            product
        }
    }
}

impl<'a> union_example::UnionExample for WasiCtx<'a> {
    fn get_tag(&self, u: &types::Reason) -> Result<types::Excuse, types::Errno> {
        println!("GET TAG: {:?}", u);
        match u {
            types::Reason::DogAte { .. } => Ok(types::Excuse::DogAte),
            types::Reason::Traffic { .. } => Ok(types::Excuse::Traffic),
            types::Reason::Sleeping { .. } => Ok(types::Excuse::Sleeping),
        }
    }
    fn reason_mult(&self, u: &types::ReasonMut<'_>, multiply_by: u32) -> Result<(), types::Errno> {
        match u {
            types::ReasonMut::DogAte(fptr) => {
                let val = fptr.read().expect("valid pointer");
                println!("REASON MULT DogAte({})", val);
                fptr.write(mult_zero_nan(val, multiply_by))
                    .expect("valid pointer");
            }
            types::ReasonMut::Traffic(iptr) => {
                let val = iptr.read().expect("valid pointer");
                println!("REASON MULT Traffic({})", val);
                iptr.write(mult_lose_overflow(val, multiply_by))
                    .expect("valid pointer");
            }
            types::ReasonMut::Sleeping => {
                println!("REASON MULT Sleeping");
            }
        }
        Ok(())
    }
}

fn reason_strat() -> impl Strategy<Value = types::Reason> {
    prop_oneof![
        prop::num::f32::ANY.prop_map(|v| types::Reason::DogAte(v)),
        prop::num::i32::ANY.prop_map(|v| types::Reason::Traffic(v)),
        Just(types::Reason::Sleeping),
    ]
    .boxed()
}

fn reason_tag(r: &types::Reason) -> types::Excuse {
    match r {
        types::Reason::DogAte { .. } => types::Excuse::DogAte,
        types::Reason::Traffic { .. } => types::Excuse::Traffic,
        types::Reason::Sleeping { .. } => types::Excuse::Sleeping,
    }
}

#[derive(Debug)]
struct GetTagExercise {
    pub input: types::Reason,
    pub input_loc: MemArea,
    pub return_loc: MemArea,
}

impl GetTagExercise {
    pub fn strat() -> BoxedStrategy<Self> {
        (
            reason_strat(),
            HostMemory::mem_area_strat(types::Reason::guest_size()),
            HostMemory::mem_area_strat(types::Excuse::guest_size()),
        )
            .prop_map(|(input, input_loc, return_loc)| GetTagExercise {
                input,
                input_loc,
                return_loc,
            })
            .prop_filter("non-overlapping pointers", |e| {
                MemArea::non_overlapping_set(&[e.input_loc, e.return_loc])
            })
            .boxed()
    }

    pub fn test(&self) {
        let ctx = WasiCtx::new();
        let host_memory = HostMemory::new();
        let bc = BorrowChecker::new();

        let discriminant: u8 = reason_tag(&self.input).into();
        host_memory
            .ptr(&bc, self.input_loc.ptr)
            .write(discriminant)
            .expect("input discriminant ptr");
        match self.input {
            types::Reason::DogAte(f) => {
                host_memory
                    .ptr(&bc, self.input_loc.ptr + 4)
                    .write(f)
                    .expect("input contents ref_mut");
            }
            types::Reason::Traffic(v) => host_memory
                .ptr(&bc, self.input_loc.ptr + 4)
                .write(v)
                .expect("input contents ref_mut"),
            types::Reason::Sleeping => {} // Do nothing
        }
        let e = union_example::get_tag(
            &ctx,
            &host_memory,
            &bc,
            self.input_loc.ptr as i32,
            self.return_loc.ptr as i32,
        );

        assert_eq!(e, types::Errno::Ok.into(), "get_tag errno");

        let return_val: types::Excuse = host_memory
            .ptr(&bc, self.return_loc.ptr)
            .read()
            .expect("return ref");

        assert_eq!(return_val, reason_tag(&self.input), "get_tag return value");
    }
}

proptest! {
    #[test]
    fn get_tag(e in GetTagExercise::strat()) {
        e.test();
    }
}

#[derive(Debug)]
struct ReasonMultExercise {
    pub input: types::Reason,
    pub input_loc: MemArea,
    pub input_pointee_loc: MemArea,
    pub multiply_by: u32,
}

impl ReasonMultExercise {
    pub fn strat() -> BoxedStrategy<Self> {
        (
            reason_strat(),
            HostMemory::mem_area_strat(types::Reason::guest_size()),
            HostMemory::mem_area_strat(4),
            prop::num::u32::ANY,
        )
            .prop_map(
                |(input, input_loc, input_pointee_loc, multiply_by)| ReasonMultExercise {
                    input,
                    input_loc,
                    input_pointee_loc,
                    multiply_by,
                },
            )
            .prop_filter("non-overlapping pointers", |e| {
                MemArea::non_overlapping_set(&[e.input_loc, e.input_pointee_loc])
            })
            .boxed()
    }

    pub fn test(&self) {
        let ctx = WasiCtx::new();
        let host_memory = HostMemory::new();
        let bc = BorrowChecker::new();

        let discriminant: u8 = reason_tag(&self.input).into();
        host_memory
            .ptr(&bc, self.input_loc.ptr)
            .write(discriminant)
            .expect("input discriminant ref_mut");
        host_memory
            .ptr(&bc, self.input_loc.ptr + 4)
            .write(self.input_pointee_loc.ptr)
            .expect("input pointer ref_mut");

        match self.input {
            types::Reason::DogAte(f) => {
                host_memory
                    .ptr(&bc, self.input_pointee_loc.ptr)
                    .write(f)
                    .expect("input contents ref_mut");
            }
            types::Reason::Traffic(v) => {
                host_memory
                    .ptr(&bc, self.input_pointee_loc.ptr)
                    .write(v)
                    .expect("input contents ref_mut");
            }
            types::Reason::Sleeping => {} // Do nothing
        }
        let e = union_example::reason_mult(
            &ctx,
            &host_memory,
            &bc,
            self.input_loc.ptr as i32,
            self.multiply_by as i32,
        );

        assert_eq!(e, types::Errno::Ok.into(), "reason_mult errno");

        match self.input {
            types::Reason::DogAte(f) => {
                let f_result: f32 = host_memory
                    .ptr(&bc, self.input_pointee_loc.ptr)
                    .read()
                    .expect("input contents ref_mut");
                assert_eq!(
                    mult_zero_nan(f, self.multiply_by),
                    f_result,
                    "DogAte result"
                )
            }
            types::Reason::Traffic(v) => {
                let v_result: i32 = host_memory
                    .ptr(&bc, self.input_pointee_loc.ptr)
                    .read()
                    .expect("input contents ref_mut");
                assert_eq!(
                    mult_lose_overflow(v, self.multiply_by),
                    v_result,
                    "Traffic result"
                )
            }
            types::Reason::Sleeping => {} // Do nothing
        }
    }
}

proptest! {
    #[test]
    fn reason_mult(e in ReasonMultExercise::strat()) {
        e.test();
    }
}
