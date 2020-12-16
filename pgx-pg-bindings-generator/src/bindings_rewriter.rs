use proc_macro2::{Ident, Span};
use quote::*;
use std::collections::HashMap;
use syn::{Attribute, FnArg, ForeignItem, GenericArgument, Item, PathArguments, ReturnType, Type};

pub struct PgBindingsRewriter {
    bindings: bindgen::Bindings,
}

impl PgBindingsRewriter {
    pub fn new(bindings: bindgen::Bindings) -> Self {
        PgBindingsRewriter { bindings }
    }

    pub fn rewrite(self) -> Result<syn::File, std::io::Error> {
        let mut file = syn::parse_file(&self.bindings.to_string())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        self.replace_type_aliases(&mut file.items);

        let mut structs = Vec::new();
        for item in file.items.iter_mut() {
            match item {
                Item::Struct(item) => {
                    self.rewrite_struct(item);
                    structs.push(item.ident.clone());
                }
                Item::ForeignMod(item) => self.rewrite_foreign_mod(item),
                Item::Type(item) => while self.rewrite_type(&mut item.ty) {},
                _ => {}
            }
        }

        for ident in structs {
            let ident_name = ident.to_string();
            if ident_name != "List" && ident_name != "RelationData" && !ident_name.starts_with("_")
            {
                file.items.push(
                    syn::parse2(quote! {
                        impl New for PgPtr<#ident> {
                            #[inline]
                            fn new() -> Self {
                                unsafe { crate::pgptr::palloc(std::mem::size_of::<#ident>()) }
                            }

                            #[inline]
                            fn new0() -> Self {
                                unsafe { crate::pgptr::palloc0(std::mem::size_of::<#ident>()) }
                            }
                        }
                    })
                    .expect("failed to implement New trait"),
                );
            }
        }

        Ok(file)
    }

    fn replace_type_aliases(&self, items: &mut Vec<syn::Item>) {
        // first, lets find all the type aliases that are pointers
        let mut aliases = HashMap::new();
        for item in items.iter_mut() {
            if let Item::Type(alias) = item {
                if let Type::Ptr(ptr) = alias.ty.as_ref() {
                    if let Type::Path(type_path) = ptr.elem.as_ref() {
                        aliases.insert(alias.ident.clone(), type_path.path.clone());

                        // // change the alias name
                        // alias.ident =
                        //     syn::Ident::new(&format!("Original{}", alias.ident), ptr.span());
                    }
                }
            }
        }

        // next walk through all the items again, and replace the aliases
        // we find in struct, extern "C" functions, statics
        for item in items.iter_mut() {
            match item {
                Item::Struct(item) => {
                    for field in item.fields.iter_mut() {
                        self.replace_alias_usage(&mut field.ty, &mut aliases);
                    }
                }
                Item::ForeignMod(item) => {
                    for fitem in item.items.iter_mut() {
                        match fitem {
                            ForeignItem::Fn(func) => {
                                // args
                                for arg in func.sig.inputs.iter_mut() {
                                    if let FnArg::Typed(arg) = arg {
                                        self.replace_alias_usage(arg.ty.as_mut(), &aliases);
                                    }
                                }

                                // return type
                                if let ReturnType::Type(_, ty) = &mut func.sig.output {
                                    self.replace_alias_usage(ty, &aliases);
                                }
                            }

                            ForeignItem::Static(s) => {
                                self.replace_alias_usage(s.ty.as_mut(), &aliases)
                            }

                            _ => {}
                        }
                    }
                }
                Item::Type(item) => self.replace_alias_usage(item.ty.as_mut(), &aliases),

                _ => {}
            }
        }
    }

    fn replace_alias_usage(&self, ty: &mut syn::Type, aliases: &HashMap<Ident, syn::Path>) {
        match ty {
            Type::Ptr(ptr) => {
                self.replace_alias_usage(ptr.elem.as_mut(), aliases);
            }
            Type::Array(_) => {}
            Type::BareFn(func) => {
                for input in func.inputs.iter_mut() {
                    self.replace_alias_usage(&mut input.ty, aliases)
                }

                if let ReturnType::Type(_, ty) = &mut func.output {
                    self.replace_alias_usage(ty.as_mut(), aliases)
                }
            }
            Type::Group(group) => {
                self.replace_alias_usage(group.elem.as_mut(), aliases);
            }
            Type::Paren(paren) => {
                self.replace_alias_usage(paren.elem.as_mut(), aliases);
            }
            Type::Path(type_path) => {
                let ident = &type_path
                    .path
                    .segments
                    .iter()
                    .next()
                    .as_ref()
                    .unwrap()
                    .ident;

                if let Some(new_path) = aliases.get(&ident) {
                    *ty = syn::parse2(quote! {*mut #new_path}).expect("failed to make ptr type");
                } else {
                    for segment in type_path.path.segments.iter_mut() {
                        match &mut segment.arguments {
                            PathArguments::None => {}
                            PathArguments::AngleBracketed(angle) => {
                                for arg in angle.args.iter_mut() {
                                    if let GenericArgument::Type(ty) = arg {
                                        self.replace_alias_usage(ty, aliases)
                                    }
                                }
                            }
                            PathArguments::Parenthesized(paren) => {
                                for input in paren.inputs.iter_mut() {
                                    self.replace_alias_usage(input, aliases)
                                }

                                if let ReturnType::Type(_, ty) = &mut paren.output {
                                    self.replace_alias_usage(ty.as_mut(), aliases)
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn rewrite_struct(&self, item: &mut syn::ItemStruct) {
        for field in item.fields.iter_mut() {
            while self.rewrite_type(&mut field.ty) {}
        }
    }

    fn rewrite_foreign_mod(&self, item: &mut syn::ItemForeignMod) {
        // add the #[pg_guard] attribute to this `extern "C"` block
        item.attrs.push(Attribute {
            pound_token: syn::token::Pound(Span::call_site()),
            style: syn::AttrStyle::Outer,
            bracket_token: Default::default(),
            path: syn::parse2(quote! {pg_guard}).expect("failed to make #[pg_guard] attribute"),
            tokens: Default::default(),
        });

        for fitem in item.items.iter_mut() {
            match fitem {
                ForeignItem::Fn(func) => {
                    for arg in func.sig.inputs.iter_mut() {
                        if let FnArg::Typed(arg) = arg {
                            while self.rewrite_type(arg.ty.as_mut()) {}
                        }
                    }

                    if let ReturnType::Type(_, ty) = &mut func.sig.output {
                        while self.rewrite_type(ty) {}
                    }
                }
                ForeignItem::Static(s) => while self.rewrite_type(s.ty.as_mut()) {},
                _ => {}
            }
        }
    }

    fn rewrite_type(&self, ty: &mut Type) -> bool {
        let mut rc = false;
        match ty {
            Type::Ptr(ptr) => {
                let elem = &ptr.elem;
                *ty = syn::parse2(quote! { PgPtr<#elem> }).expect("failed to make PgPtr type");
                return true;
            }
            Type::BareFn(func) => {
                for input in func.inputs.iter_mut() {
                    rc |= self.rewrite_type(&mut input.ty)
                }

                if let ReturnType::Type(_, ty) = &mut func.output {
                    rc |= self.rewrite_type(ty.as_mut())
                }
            }
            Type::Group(group) => {
                rc |= self.rewrite_type(group.elem.as_mut());
            }
            Type::Paren(paren) => {
                rc |= self.rewrite_type(paren.elem.as_mut());
            }
            Type::Path(path) => {
                for segment in path.path.segments.iter_mut() {
                    match &mut segment.arguments {
                        PathArguments::None => {}
                        PathArguments::AngleBracketed(angle) => {
                            for arg in angle.args.iter_mut() {
                                if let GenericArgument::Type(ty) = arg {
                                    rc |= self.rewrite_type(ty)
                                }
                            }
                        }
                        PathArguments::Parenthesized(paren) => {
                            for input in paren.inputs.iter_mut() {
                                rc |= self.rewrite_type(input)
                            }

                            if let ReturnType::Type(_, ty) = &mut paren.output {
                                rc |= self.rewrite_type(ty.as_mut())
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        rc
    }
}
