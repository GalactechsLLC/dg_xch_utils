extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Index, parse_macro_input};

#[proc_macro_derive(ChiaSerial)]
pub fn derive_chia_serial(input: TokenStream) -> TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let generics = input.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let name = input.ident;
    let (to_bytes, from_bytes) = create_to_bytes(&input.data);
    let from_sexp = create_sexp_from(input.data);
    let core = resolve_crate_path("dg_xch_core");
    let generated = quote! {
        impl #impl_generics dg_xch_serialize::ChiaSerialize for #name #ty_generics #where_clause {
            fn to_bytes(&self, macro_chia_protocol_version: dg_xch_serialize::ChiaProtocolVersion) -> Result<Vec<u8>, std::io::Error> {
                let mut bytes = Vec::new();
                dg_xch_serialize::ChiaSerialize::append_bytes(self, &mut bytes, macro_chia_protocol_version)?;
                Ok(bytes)
            }
            fn append_bytes(&self, bytes: &mut Vec<u8>, macro_chia_protocol_version: dg_xch_serialize::ChiaProtocolVersion) -> Result<(), std::io::Error> {
                #to_bytes
            }
            fn from_bytes(bytes: &mut std::io::Cursor<&[u8]>, macro_chia_protocol_version: dg_xch_serialize::ChiaProtocolVersion) -> Result<Self, std::io::Error>
            where
                Self: Sized,
            {
                #from_bytes
            }
        }
        impl From<&#name> for #core::clvm::sexp::SExp<'static> {
            fn from(val: &#name) -> #core::clvm::sexp::SExp<'static> {
                #from_sexp
            }
        }
        impl From<#name> for #core::clvm::sexp::SExp<'static> {
            fn from(val: #name) -> #core::clvm::sexp::SExp<'static> {
                (&val).into()
            }
        }
    };
    generated.into()
}

fn create_to_bytes(data: &Data) -> (TokenStream2, TokenStream2) {
    match data {
        Data::Struct(s) => {
            match s.fields {
                Fields::Named(ref fields) => {
                    let to_bytes = fields.named.iter().map(|f| {
                        let name = &f.ident;
                        quote_spanned! {f.span()=>
                            dg_xch_serialize::ChiaSerialize::append_bytes(&self.#name, bytes, macro_chia_protocol_version)?;
                        }
                    });
                    let names = fields.named.iter().map(|f| {
                        let name = &f.ident;
                        quote_spanned! {f.span()=>
                            let #name = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, macro_chia_protocol_version)?;
                        }
                    });
                    let assign = fields.named.iter().map(|f| {
                        let name = &f.ident;
                        quote_spanned! {f.span()=>
                            #name: #name,
                        }
                    });
                    (
                        quote! {
                            #(#to_bytes)*
                            Ok(())
                        },
                        quote! {
                            #(#names)*
                            Ok(Self {
                                #(#assign)*
                            })
                        },
                    )
                }
                Fields::Unnamed(ref fields) => {
                    let to_bytes = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let index = Index::from(i);
                        quote_spanned! {f.span()=>
                            dg_xch_serialize::ChiaSerialize::append_bytes(&self.#index, bytes, macro_chia_protocol_version)?;
                        }
                    });

                    let names = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let name_ident = format_ident!("s_{}", i);
                        quote_spanned! {f.span()=>
                            let #name_ident = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, macro_chia_protocol_version)?;
                        }
                    });

                    let assign = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let index = Index::from(i);
                        let name_ident = format_ident!("s_{}", i);
                        quote_spanned! {f.span()=>
                            #index: #name_ident,
                        }
                    });

                    (
                        quote! {
                            #(#to_bytes)*
                            Ok(())
                        },
                        quote! {
                            #(#names)*
                            Ok(Self {
                                #(#assign)*
                            })
                        },
                    )
                }
                Fields::Unit => {
                    // Unit structs cannot own more than 0 bytes of heap memory.
                    todo!()
                }
            }
        }
        Data::Enum(e) => (
            quote_spanned! {e.enum_token.span()=>
                bytes.push(*self as u8);
                Ok(())
            },
            quote_spanned! {e.enum_token.span()=>
                use std::io::Read;
                let mut enum_buf: [u8; 1] = [0; 1];
                bytes.read_exact(&mut enum_buf)?;
                Ok(enum_buf[0].into())
            },
        ),
        Data::Union(_u) => {
            todo!()
        }
    }
}

fn create_sexp_from(data: Data) -> TokenStream2 {
    let core = resolve_crate_path("dg_xch_core");
    match data {
        Data::Struct(s) => {
            match s.fields {
                Fields::Named(ref fields) => {
                    if fields.named.is_empty() {
                        quote! {
                            #core::constants::NULL_SEXP
                        }
                    } else {
                        let to_sexp = fields.named.iter().map(|f| {
                            let name = &f.ident;
                            quote_spanned! {f.span()=>
                                #core::clvm::sexp::SExp::from(&val.#name),
                            }
                        });
                        quote! {
                            (&[
                                #(#to_sexp)*
                            ]).into()
                        }
                    }
                }
                Fields::Unnamed(ref fields) => {
                    let to_sexp = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let index = Index::from(i);
                        quote_spanned! {f.span()=>
                            #core::clvm::sexp::SExp::from(&val.#index),
                        }
                    });
                    quote! {
                        (&[
                            #(#to_sexp)*
                        ]).into()
                    }
                }
                Fields::Unit => {
                    // Unit structs cannot own more than 0 bytes of heap memory.
                    todo!()
                }
            }
        }
        Data::Enum(e) => quote_spanned! {e.enum_token.span()=>
            #core::clvm::sexp::SExp::from(*val as u8)
        },
        Data::Union(_u) => {
            todo!()
        }
    }
}

fn resolve_crate_path(wanted: &str) -> TokenStream2 {
    match crate_name(wanted) {
        Ok(FoundCrate::Itself) => {
            // Caller is the same crate we’re targeting (e.g. tests inside that crate)
            let ident = format_ident!("crate");
            quote!(#ident)
        }
        Ok(FoundCrate::Name(actual)) => {
            // Caller renamed the crate; use the actual name
            let ident = format_ident!("{}", actual);
            quote!(::#ident)
        }
        Err(_) => {
            // Fallback: assume the published name is usable
            let ident = format_ident!("{}", wanted);
            quote!(::#ident)
        }
    }
}
