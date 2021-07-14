use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::{quote, ToTokens, TokenStreamExt};

#[derive(Debug, Clone)]
pub struct PostgresType {
    name: Ident,
    in_fn: Ident,
    out_fn: Ident,
}

impl PostgresType {
    pub fn new(name: Ident, in_fn: Ident, out_fn: Ident) -> Self {
        Self {
            name,
            in_fn,
            out_fn,
        }
    }
}

impl ToTokens for PostgresType {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let name = &self.name;
        let in_fn = &self.in_fn;
        let out_fn = &self.out_fn;
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
                crate::__pgx_internals::PostgresType(pgx_utils::pg_inventory::InventoryPostgresType {
                    name: stringify!(#name),
                    file: file!(),
                    line: line!(),
                    module_path: module_path!(),
                    full_path: core::any::type_name::<#name>(),
                    id: *<#name as WithTypeIds>::ITEM_ID,
                    option_id: *<#name as WithTypeIds>::OPTION_ID,
                    vec_id: *<#name as WithTypeIds>::VEC_ID,
                    vec_option_id: *<#name as WithTypeIds>::VEC_OPTION_ID,
                    array_id: *WithArrayTypeId::<#name>::ARRAY_ID,
                    option_array_id: *WithArrayTypeId::<#name>::OPTION_ARRAY_ID,
                    varlena_id: *WithVarlenaTypeId::<#name>::VARLENA_ID,
                    in_fn: stringify!(#in_fn),
                    in_fn_module_path: {
                        let in_fn = stringify!(#in_fn);
                        let mut path_items: Vec<_> = in_fn.split("::").collect();
                        let _ = path_items.pop(); // Drop the one we don't want.
                        path_items.join("::")
                    },
                    out_fn: stringify!(#out_fn),
                    out_fn_module_path: {
                        let out_fn = stringify!(#out_fn);
                        let mut path_items: Vec<_> = out_fn.split("::").collect();
                        let _ = path_items.pop(); // Drop the one we don't want.
                        path_items.join("::")
                    }
                })
            }
        };
        tokens.append_all(inv);
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InventoryPostgresType {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub full_path: &'static str,
    pub module_path: &'static str,
    pub id: core::any::TypeId,
    pub option_id: core::any::TypeId,
    pub vec_id: core::any::TypeId,
    pub vec_option_id: core::any::TypeId,
    pub array_id: Option<core::any::TypeId>,
    pub option_array_id: Option<core::any::TypeId>,
    pub varlena_id: Option<core::any::TypeId>,
    pub in_fn: &'static str,
    pub in_fn_module_path: String,
    pub out_fn: &'static str,
    pub out_fn_module_path: String,
}

impl InventoryPostgresType {
    pub fn id_matches(&self, candidate: &core::any::TypeId) -> bool {
        *candidate == self.id
            || *candidate == self.option_id
            || *candidate == self.vec_id
            || *candidate == self.vec_option_id
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
