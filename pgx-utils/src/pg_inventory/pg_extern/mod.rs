mod argument;
mod attribute;
mod operator;
mod returning;
mod search_path;

use argument::Argument;
use attribute::PgxAttributes;
use operator::{PgxOperator, PgxOperatorAttributeWithIdent, PgxOperatorOpName};
use returning::Returning;
use search_path::SearchPathList;

use eyre::WrapErr;
use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens, TokenStreamExt};
use std::convert::TryFrom;
use syn::parse::{Parse, ParseStream};
use syn::Meta;

pub use argument::InventoryPgExternInput;
pub use operator::InventoryPgOperator;
pub use returning::InventoryPgExternReturn;

#[derive(Debug, Clone)]
pub struct PgExtern {
    attrs: PgxAttributes,
    func: syn::ItemFn,
}

impl PgExtern {
    fn extern_attrs(&self) -> &PgxAttributes {
        &self.attrs
    }

    fn overridden(&self) -> Option<String> {
        let mut retval = None;
        let mut in_commented_sql_block = false;
        for attr in &self.func.attrs {
            let meta = attr.parse_meta().ok();
            if let Some(meta) = meta {
                if meta.path().is_ident("doc") {
                    let content = match meta {
                        Meta::Path(_) | Meta::List(_) => continue,
                        Meta::NameValue(mnv) => mnv,
                    };
                    if let syn::Lit::Str(inner) = content.lit {
                        if !in_commented_sql_block && inner.value().trim() == "```pgxsql" {
                            in_commented_sql_block = true;
                        } else if in_commented_sql_block && inner.value().trim() == "```" {
                            in_commented_sql_block = false;
                        } else if in_commented_sql_block {
                            let sql = retval.get_or_insert_with(String::default);
                            let line = inner.value().trim_start().replace(
                                "@FUNCTION_NAME@",
                                &*(self.func.sig.ident.to_string() + "_wrapper"),
                            ) + "\n";
                            sql.push_str(&*line);
                        }
                    }
                }
            }
        }
        retval
    }

    fn operator(&self) -> Option<PgxOperator> {
        let mut skel = Option::<PgxOperator>::default();
        for attr in &self.func.attrs {
            let last_segment = attr.path.segments.last().unwrap();
            match last_segment.ident.to_string().as_str() {
                "opname" => {
                    let attr: PgxOperatorOpName = syn::parse2(attr.tokens.clone())
                        .expect(&format!("Unable to parse {:?}", &attr.tokens));
                    skel.get_or_insert_with(Default::default)
                        .opname
                        .get_or_insert(attr);
                }
                "commutator" => {
                    let attr: PgxOperatorAttributeWithIdent = syn::parse2(attr.tokens.clone())
                        .expect(&format!("Unable to parse {:?}", &attr.tokens));
                    skel.get_or_insert_with(Default::default)
                        .commutator
                        .get_or_insert(attr);
                }
                "negator" => {
                    let attr: PgxOperatorAttributeWithIdent = syn::parse2(attr.tokens.clone())
                        .expect(&format!("Unable to parse {:?}", &attr.tokens));
                    skel.get_or_insert_with(Default::default)
                        .negator
                        .get_or_insert(attr);
                }
                "join" => {
                    let attr: PgxOperatorAttributeWithIdent = syn::parse2(attr.tokens.clone())
                        .expect(&format!("Unable to parse {:?}", &attr.tokens));
                    skel.get_or_insert_with(Default::default)
                        .join
                        .get_or_insert(attr);
                }
                "restrict" => {
                    let attr: PgxOperatorAttributeWithIdent = syn::parse2(attr.tokens.clone())
                        .expect(&format!("Unable to parse {:?}", &attr.tokens));
                    skel.get_or_insert_with(Default::default)
                        .restrict
                        .get_or_insert(attr);
                }
                "hashes" => {
                    skel.get_or_insert_with(Default::default).hashes = true;
                }
                "merges" => {
                    skel.get_or_insert_with(Default::default).merges = true;
                }
                _ => (),
            }
        }
        skel
    }

    fn search_path(&self) -> Option<SearchPathList> {
        self.func
            .attrs
            .iter()
            .find(|f| {
                f.path
                    .segments
                    .first()
                    .map(|f| f.ident == Ident::new("search_path", Span::call_site()))
                    .unwrap_or_default()
            })
            .and_then(|attr| Some(attr.parse_args::<SearchPathList>().unwrap()))
    }

    fn inputs(&self) -> eyre::Result<Vec<Argument>> {
        let mut args = Vec::default();
        for input in &self.func.sig.inputs {
            let arg = Argument::build(input.clone())
                .wrap_err_with(|| format!("Could not map {:?}", input))?;
            if let Some(arg) = arg {
                args.push(arg);
            }
        }
        Ok(args)
    }

    fn returns(&self) -> Returning {
        Returning::try_from(&self.func.sig.output).unwrap()
    }

    pub fn new(attr: TokenStream2, item: TokenStream2) -> Result<Self, syn::Error> {
        let attrs = syn::parse2::<PgxAttributes>(attr)?;
        let func = syn::parse2::<syn::ItemFn>(item)?;
        Ok(Self { attrs, func })
    }
}

impl ToTokens for PgExtern {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let ident = &self.func.sig.ident;
        let extern_attrs = self.extern_attrs();
        let search_path = self.search_path().into_iter();
        let inputs = self.inputs().unwrap();
        let returns = self.returns();
        let operator = self.operator().into_iter();
        let overridden = self.overridden().into_iter();

        let inv = quote! {
            pgx_utils::pg_inventory::inventory::submit! {
                use core::any::TypeId;
                crate::__pgx_internals::PgExtern(pgx_utils::pg_inventory::InventoryPgExtern {
                    name: stringify!(#ident),
                    file: file!(),
                    line: line!(),
                    module_path: core::module_path!(),
                    full_path: concat!(core::module_path!(), "::", stringify!(#ident)),
                    extern_attrs: #extern_attrs,
                    search_path: None#( .unwrap_or(Some(vec![#search_path])) )*,
                    fn_args: vec![#(#inputs),*],
                    fn_return: #returns,
                    operator: None#( .unwrap_or(Some(#operator)) )*,
                    overridden: None#( .unwrap_or(Some(#overridden)) )*,
                })
            }
        };
        tokens.append_all(inv);
    }
}

impl Parse for PgExtern {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        Ok(Self {
            attrs: input.parse()?,
            func: input.parse()?,
        })
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InventoryPgExtern {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub module_path: &'static str,
    pub full_path: &'static str,
    pub extern_attrs: Vec<crate::ExternArgs>,
    pub search_path: Option<Vec<&'static str>>,
    pub fn_args: Vec<InventoryPgExternInput>,
    pub fn_return: InventoryPgExternReturn,
    pub operator: Option<InventoryPgOperator>,
    pub overridden: Option<&'static str>,
}
