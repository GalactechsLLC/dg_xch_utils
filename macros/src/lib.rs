extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Index};

#[proc_macro_derive(ChiaSerial)]
pub fn derive_chia_serial(input: TokenStream) -> TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let name = input.ident;
    let (to_bytes, from_bytes) = create_to_bytes(input.data);
    let gen = quote! {
        impl dg_xch_serialize::ChiaSerialize for #name {
            fn to_bytes(&self, macro_chia_protocol_version: dg_xch_serialize::ChiaProtocolVersion) -> Vec<u8> {
                #to_bytes
            }
            fn from_bytes<T: AsRef<[u8]>>(bytes: &mut std::io::Cursor<T>, macro_chia_protocol_version: dg_xch_serialize::ChiaProtocolVersion) -> Result<Self, std::io::Error>
            where
                Self: Sized,
            {
                #from_bytes
            }
        }
    };
    gen.into()
}

fn create_to_bytes(data: Data) -> (TokenStream2, TokenStream2) {
    match data {
        Data::Struct(s) => {
            match s.fields {
                Fields::Named(ref fields) => {
                    let to_bytes = fields.named.iter().map(|f| {
                        let name = &f.ident;
                        quote_spanned! {f.span()=>
                            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(&self.#name, macro_chia_protocol_version));
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
                            let mut bytes = vec![];
                            #(#to_bytes)*
                            bytes
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
                            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(&self.#index, macro_chia_protocol_version));
                        }
                    });
                    let names = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let index = Index::from(i);
                        quote_spanned! {f.span()=>
                            let #index = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, macro_chia_protocol_version)?;
                        }
                    });
                    let assign = fields.unnamed.iter().enumerate().map(|(i, f)| {
                        let index = Index::from(i);
                        quote_spanned! {f.span()=>
                            #index: #index,
                        }
                    });
                    (
                        quote! {
                            #(#to_bytes)*
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
                vec![*self as u8]
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
