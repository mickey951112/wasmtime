extern crate proc_macro;

use proc_macro::TokenStream;
use syn::parse_macro_input;

/// This macro expands to a set of `pub` Rust modules:
///
/// * The `types` module contains definitions for each `typename` declared in
///   the witx document. Type names are translated to the Rust-idiomatic
///   CamelCase.
///
/// * For each `module` defined in the witx document, a Rust module is defined
///   containing definitions for that module. Module names are teanslated to the
///   Rust-idiomatic snake\_case.
///
///     * For each `@interface func` defined in a witx module, an abi-level
///       function is generated which takes ABI-level arguments, along with a
///       "context" struct (whose type is given by the `ctx` field in the
///       macro invocation) and a `GuestMemory` implementation.
///
///     * A public "module trait" is defined (called the module name, in
///       SnakeCase) which has a `&self` method for each function in the
///       module. These methods takes idiomatic Rust types for each argument
///       and return `Result<($return_types),$error_type>`
///
/// Arguments are provided using Rust struct value syntax.
///
/// * `witx` takes a list of string literal paths. Paths are relative to the
///   CARGO_MANIFEST_DIR of the crate where the macro is invoked.
/// * `ctx` takes a type name. This type must implement all of the module
///    traits
///
/// ## Example
///
/// ```
/// use wiggle_runtime::{GuestPtr, GuestErrorType};
///
/// /// The test witx file `arrays.witx` lives in the test directory. For a
/// /// full-fledged example with runtime tests, see `tests/arrays.rs` and
/// /// the rest of the files in that directory.
/// wiggle::from_witx!({
///     witx: ["tests/arrays.witx"],
///     ctx: YourCtxType,
/// });
///
/// /// The `ctx` type for this wiggle invocation.
/// pub struct YourCtxType {}
///
/// /// `arrays.witx` contains one module called `arrays`. So, we must
/// /// implement this one method trait for our ctx type:
/// impl arrays::Arrays for YourCtxType {
///     /// The arrays module has two methods, shown here.
///     /// Note that the `GuestPtr` type comes from `wiggle_runtime`,
///     /// whereas the witx-defined types like `Excuse` and `Errno` come
///     /// from the `pub mod types` emitted by the `wiggle::from_witx!`
///     /// invocation above.
///     fn reduce_excuses(&self, _a: &GuestPtr<[GuestPtr<types::Excuse>]>)
///         -> Result<types::Excuse, types::Errno> {
///         unimplemented!()
///     }
///     fn populate_excuses(&self, _a: &GuestPtr<[GuestPtr<types::Excuse>]>)
///         -> Result<(), types::Errno> {
///         unimplemented!()
///     }
/// }
///
/// /// For all types used in the `Error` position of a `Result` in the module
/// /// traits, you must implement `GuestErrorType` which tells wiggle-generated
/// /// code how to determine if a method call has been successful, as well as
/// /// how to translate a wiggle runtime error into an ABI-level error.
/// impl<'a> GuestErrorType<'a> for types::Errno {
///     type Context = YourCtxType;
///     fn success() -> Self {
///         unimplemented!()
///     }
///     fn from_error(_e: wiggle_runtime::GuestError, _c: &Self::Context) -> Self {
///         unimplemented!()
///     }
/// }
///
/// # fn main() { println!("this fools doc tests into compiling the above outside a function body")
/// # }
/// ```
#[proc_macro]
pub fn from_witx(args: TokenStream) -> TokenStream {
    let mut config = parse_macro_input!(args as wiggle_generate::Config);
    config.witx.make_paths_relative_to(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR env var"),
    );

    #[cfg(feature = "wiggle_metadata")]
    {
        config.emit_metadata = true;
    }

    let doc = witx::load(&config.witx.paths).expect("loading witx");
    TokenStream::from(wiggle_generate::generate(&doc, &config))
}
