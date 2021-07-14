use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, ToTokens, TokenStreamExt};
use syn::{punctuated::Punctuated, Ident, Token};

#[derive(Debug, Clone)]
pub struct PostgresEnum {
    pub name: Ident,
    pub variants: Punctuated<syn::Variant, Token![,]>,
}

impl PostgresEnum {
    pub fn new(name: Ident, variants: Punctuated<syn::Variant, Token![,]>) -> Self {
        Self { name, variants }
    }
}

impl ToTokens for PostgresEnum {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let name = &self.name;
        let variants = self.variants.iter();
        let inv = quote! {
            pgx_utils::pg_inventory::inventory::submit! {
                use core::{mem::MaybeUninit, any::{TypeId, Any}, marker::PhantomData};
                use ::pgx::datum::{
                    WithTypeIds,
                    WithoutArrayTypeId,
                    WithVarlenaTypeId,
                    WithArrayTypeId,
                    WithoutVarlenaTypeId
                };
                crate::__pgx_internals::PostgresEnum(pgx_utils::pg_inventory::InventoryPostgresEnum {
                    name: stringify!(#name),
                    file: file!(),
                    line: line!(),
                    module_path: module_path!(),
                    full_path: core::any::type_name::<#name>(),
                    id: *<#name as WithTypeIds>::ITEM_ID,
                    option_id: *<#name as WithSizedTypeIds>::OPTION_ID,
                    vec_id: *<#name as WithSizedTypeIds>::VEC_ID,
                    vec_option_id: *<#name as WithSizedTypeIds>::VEC_OPTION_ID,
                    array_id: *WithArrayTypeId::<#name>::ARRAY_ID,
                    option_array_id: *WithArrayTypeId::<#name>::OPTION_ARRAY_ID,
                    varlena_id: *WithVarlenaTypeId::<#name>::VARLENA_ID,
                    variants: vec![ #(  stringify!(#variants)  ),* ],
                })
            }
        };
        tokens.append_all(inv);
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InventoryPostgresEnum {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub full_path: &'static str,
    pub module_path: &'static str,
    pub id: core::any::TypeId,
    pub option_id: Option<core::any::TypeId>,
    pub vec_id: Option<core::any::TypeId>,
    pub vec_option_id: Option<core::any::TypeId>,
    pub array_id: Option<core::any::TypeId>,
    pub option_array_id: Option<core::any::TypeId>,
    pub varlena_id: Option<core::any::TypeId>,
    pub variants: Vec<&'static str>,
}

impl InventoryPostgresEnum {
    pub fn id_matches(&self, candidate: &core::any::TypeId) -> bool {
        *candidate == self.id
            || if let Some(option_id) = self.option_id {
                *candidate == option_id
            } else { false }
            || if let Some(vec_id) = self.vec_id {
                *candidate == vec_id
            } else { false }
            || if let Some(vec_option_id) = self.vec_option_id {
                *candidate == vec_option_id
            } else {
                false
            }
            || if let Some(array_id) = self.array_id {
                *candidate == array_id
            } else {
                false
            }
            || if let Some(option_array_id) = self.option_array_id {
                *candidate == option_array_id
            } else {
                false
            }
            || if let Some(varlena_id) = self.varlena_id {
                *candidate == varlena_id
            } else {
                false
            }
    }
}
