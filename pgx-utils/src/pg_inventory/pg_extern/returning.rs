use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, ToTokens, TokenStreamExt};
use std::{convert::TryFrom, ops::Deref};
use syn::{
    parse::{Parse, ParseStream},
    Token,
};

#[derive(Debug, Clone)]
pub enum Returning {
    None,
    Type(syn::Type),
    SetOf(syn::TypePath),
    Iterated(Vec<(syn::Type, Option<String>)>),
    /// `pgx_pg_sys::Datum`
    Trigger,
}

impl TryFrom<&syn::ReturnType> for Returning {
    type Error = ();

    fn try_from(value: &syn::ReturnType) -> Result<Self, Self::Error> {
        Ok(match &value {
            syn::ReturnType::Default => Returning::None,
            syn::ReturnType::Type(_, ty) => match *ty.clone() {
                syn::Type::ImplTrait(impl_trait) => match impl_trait.bounds.first().unwrap() {
                    syn::TypeParamBound::Trait(trait_bound) => {
                        let last_path_segment = trait_bound.path.segments.last().unwrap();
                        match last_path_segment.ident.to_string().as_str() {
                            "Iterator" => match &last_path_segment.arguments {
                                syn::PathArguments::AngleBracketed(args) => {
                                    match args.args.first().unwrap() {
                                        syn::GenericArgument::Binding(binding) => match &binding.ty
                                        {
                                            syn::Type::Tuple(tuple_type) => {
                                                let returns: Vec<(syn::Type, Option<_>)> = tuple_type.elems.iter().flat_map(|elem| {
                                                    match elem {
                                                        syn::Type::Macro(macro_pat) => {
                                                            let mac = &macro_pat.mac;
                                                            let archetype = mac.path.segments.last().unwrap();
                                                            match archetype.ident.to_string().as_str() {
                                                                "name" => {
                                                                    let out: NameMacro = mac.parse_body().expect(&*format!("{:?}", mac));
                                                                    Some((out.ty, Some(out.ident)))
                                                                },
                                                                _ => unimplemented!("Don't support anything other than name."),
                                                            }
                                                        },
                                                        ty => Some((ty.clone(), None)),
                                                    }
                                                }).collect();
                                                Returning::Iterated(returns)
                                            }
                                            syn::Type::Path(path) => Returning::SetOf(path.clone()),
                                            syn::Type::Reference(type_ref) => {
                                                match &*type_ref.elem {
                                                    syn::Type::Path(path) => {
                                                        Returning::SetOf(path.clone())
                                                    }
                                                    _ => unimplemented!("Expected path"),
                                                }
                                            }
                                            ty => unimplemented!(
                                                "Only iters with tuples, got {:?}.",
                                                ty
                                            ),
                                        },
                                        _ => unimplemented!(),
                                    }
                                }
                                _ => unimplemented!(),
                            },
                            _ => unimplemented!(),
                        }
                    }
                    _ => Returning::None,
                },
                syn::Type::Path(typepath) => {
                    let path = &typepath.path;
                    let mut saw_pgx_pg_sys = false;
                    let mut saw_datum = false;

                    for segment in &path.segments {
                        if segment.ident.to_string() == "pg_sys" {
                            saw_pgx_pg_sys = true;
                        }
                        if segment.ident.to_string() == "Datum" {
                            saw_datum = true;
                        }
                    }
                    if (saw_datum && saw_pgx_pg_sys) || (saw_datum && path.segments.len() == 1) {
                        Returning::Trigger
                    } else {
                        Returning::Type(ty.deref().clone())
                    }
                }
                _ => Returning::Type(ty.deref().clone()),
            },
        })
    }
}

impl ToTokens for Returning {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let quoted = match self {
            Returning::None => quote! {
                pgx_utils::pg_inventory::InventoryPgExternReturn::None
            },
            Returning::Type(ty) => quote! {
                pgx_utils::pg_inventory::InventoryPgExternReturn::Type {
                    id: TypeId::of::<#ty>(),
                    full_path: core::any::type_name::<#ty>(),
                    module_path: {
                        let type_name = core::any::type_name::<#ty>();
                        let mut path_items: Vec<_> = type_name.split("::").collect();
                        let _ = path_items.pop(); // Drop the one we don't want.
                        path_items.join("::")
                    },
                }
            },
            Returning::SetOf(ty) => quote! {
                pgx_utils::pg_inventory::InventoryPgExternReturn::SetOf {
                    id: TypeId::of::<#ty>(),
                    full_path: core::any::type_name::<#ty>(),
                    module_path: {
                        let type_name = core::any::type_name::<#ty>();
                        let mut path_items: Vec<_> = type_name.split("::").collect();
                        let _ = path_items.pop(); // Drop the one we don't want.
                        path_items.join("::")
                    }
                }
            },
            Returning::Iterated(items) => {
                let quoted_items = items
                    .iter()
                    .map(|(ty, name)| {
                        let name_iter = name.iter();
                        quote! {
                            (
                                TypeId::of::<#ty>(),
                                core::any::type_name::<#ty>(),
                                {
                                    let type_name = core::any::type_name::<#ty>();
                                    let mut path_items: Vec<_> = type_name.split("::").collect();
                                    let _ = path_items.pop(); // Drop the one we don't want.
                                    path_items.join("::")
                                },
                                None#( .unwrap_or(Some(stringify!(#name_iter))) )*,
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                quote! {
                    pgx_utils::pg_inventory::InventoryPgExternReturn::Iterated(vec![
                        #(#quoted_items),*
                    ])
                }
            }
            Returning::Trigger => quote! {
                pgx_utils::pg_inventory::InventoryPgExternReturn::Trigger
            },
        };
        tokens.append_all(quoted);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NameMacro {
    pub(crate) ident: String,
    comma: Token![,],
    pub(crate) ty: syn::Type,
}

impl Parse for NameMacro {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        Ok(Self {
            ident: input
                .parse::<syn::Ident>()
                .map(|v| v.to_string())
                // Avoid making folks unable to use rust keywords.
                .or_else(|_| {
                    input
                        .parse::<syn::Token![type]>()
                        .map(|_| String::from("type"))
                })
                .or_else(|_| {
                    input
                        .parse::<syn::Token![mod]>()
                        .map(|_| String::from("mod"))
                })
                .or_else(|_| {
                    input
                        .parse::<syn::Token![extern]>()
                        .map(|_| String::from("extern"))
                })
                .or_else(|_| {
                    input
                        .parse::<syn::Token![async]>()
                        .map(|_| String::from("async"))
                })
                .or_else(|_| {
                    input
                        .parse::<syn::Token![crate]>()
                        .map(|_| String::from("crate"))
                })
                .or_else(|_| {
                    input
                        .parse::<syn::Token![use]>()
                        .map(|_| String::from("use"))
                })?,
            comma: input.parse()?,
            ty: input.parse()?,
        })
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum InventoryPgExternReturn {
    None,
    Type {
        id: core::any::TypeId,
        full_path: &'static str,
        module_path: String,
    },
    SetOf {
        id: core::any::TypeId,
        full_path: &'static str,
        module_path: String,
    },
    Iterated(
        Vec<(
            core::any::TypeId,
            &'static str,
            String,
            Option<&'static str>,
        )>,
    ),
    Trigger,
}
