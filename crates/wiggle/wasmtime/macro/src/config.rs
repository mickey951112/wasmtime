use {
    proc_macro2::Span,
    syn::{
        braced,
        parse::{Parse, ParseStream},
        punctuated::Punctuated,
        Error, Ident, Result, Token,
    },
    wiggle_generate::config::{CtxConf, WitxConf},
};

#[derive(Debug, Clone)]
pub struct Config {
    pub witx: WitxConf,
    pub ctx: CtxConf,
    pub instance_typename: InstanceTypenameConf,
}

#[derive(Debug, Clone)]
pub enum ConfigField {
    Witx(WitxConf),
    Ctx(CtxConf),
    InstanceTypename(InstanceTypenameConf),
}

mod kw {
    syn::custom_keyword!(witx);
    syn::custom_keyword!(witx_literal);
    syn::custom_keyword!(ctx);
    syn::custom_keyword!(instance);
}

impl Parse for ConfigField {
    fn parse(input: ParseStream) -> Result<Self> {
        let lookahead = input.lookahead1();
        if lookahead.peek(kw::witx) {
            input.parse::<kw::witx>()?;
            input.parse::<Token![:]>()?;
            Ok(ConfigField::Witx(WitxConf::Paths(input.parse()?)))
        } else if lookahead.peek(kw::witx_literal) {
            input.parse::<kw::witx_literal>()?;
            input.parse::<Token![:]>()?;
            Ok(ConfigField::Witx(WitxConf::Literal(input.parse()?)))
        } else if lookahead.peek(kw::ctx) {
            input.parse::<kw::ctx>()?;
            input.parse::<Token![:]>()?;
            Ok(ConfigField::Ctx(input.parse()?))
        } else if lookahead.peek(kw::instance) {
            input.parse::<kw::instance>()?;
            input.parse::<Token![:]>()?;
            Ok(ConfigField::InstanceTypename(input.parse()?))
        } else {
            Err(lookahead.error())
        }
    }
}

impl Config {
    pub fn build(fields: impl Iterator<Item = ConfigField>, err_loc: Span) -> Result<Self> {
        let mut witx = None;
        let mut ctx = None;
        let mut instance_typename = None;
        for f in fields {
            match f {
                ConfigField::Witx(c) => {
                    if witx.is_some() {
                        return Err(Error::new(err_loc, "duplicate `witx` field"));
                    }
                    witx = Some(c);
                }
                ConfigField::Ctx(c) => {
                    if ctx.is_some() {
                        return Err(Error::new(err_loc, "duplicate `ctx` field"));
                    }
                    ctx = Some(c);
                }
                ConfigField::InstanceTypename(c) => {
                    if instance_typename.is_some() {
                        return Err(Error::new(err_loc, "duplicate `instance` field"));
                    }
                    instance_typename = Some(c);
                }
            }
        }
        Ok(Config {
            witx: witx
                .take()
                .ok_or_else(|| Error::new(err_loc, "`witx` field required"))?,
            ctx: ctx
                .take()
                .ok_or_else(|| Error::new(err_loc, "`ctx` field required"))?,
            instance_typename: instance_typename
                .take()
                .ok_or_else(|| Error::new(err_loc, "`instance` field required"))?,
        })
    }

    /// Load the `witx` document for the configuration.
    ///
    /// # Panics
    ///
    /// This method will panic if the paths given in the `witx` field were not valid documents.
    pub fn load_document(&self) -> witx::Document {
        self.witx.load_document()
    }
}

impl Parse for Config {
    fn parse(input: ParseStream) -> Result<Self> {
        let contents;
        let _lbrace = braced!(contents in input);
        let fields: Punctuated<ConfigField, Token![,]> =
            contents.parse_terminated(ConfigField::parse)?;
        Ok(Config::build(fields.into_iter(), input.span())?)
    }
}

#[derive(Debug, Clone)]
pub struct InstanceTypenameConf {
    pub name: Ident,
}

impl Parse for InstanceTypenameConf {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(InstanceTypenameConf {
            name: input.parse()?,
        })
    }
}
