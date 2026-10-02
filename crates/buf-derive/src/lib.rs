//! `#[derive(Encode, Decode)]`.
//!
//! Structs encode their fields in declaration order. A field marked `#[var]`
//! uses VarInt/VarLong encoding via `EncodeVar`/`DecodeVar`.
//!
//! Enums encode a VarInt discriminant followed by the variant's fields.
//! Explicit discriminants (`A = 3`) are honoured, otherwise the variant index
//! is used. `#[discriminant(u8)]` (or any integer type) on the enum encodes the
//! discriminant as that fixed-width type instead of a VarInt.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Expr, Fields, Ident, Type, parse_macro_input};

#[proc_macro_derive(Encode, attributes(var, discriminant))]
pub fn derive_encode(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_encode(&input).unwrap_or_else(|e| e.to_compile_error()).into()
}

#[proc_macro_derive(Decode, attributes(var, discriminant))]
pub fn derive_decode(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_decode(&input).unwrap_or_else(|e| e.to_compile_error()).into()
}

fn is_var(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("var"))
}

fn discriminant_type(input: &DeriveInput) -> syn::Result<Option<Type>> {
    for attr in &input.attrs {
        if attr.path().is_ident("discriminant") {
            return attr.parse_args::<Type>().map(Some);
        }
    }
    Ok(None)
}

/// Discriminant values for each variant, following Rust's implicit numbering.
fn discriminants(data: &syn::DataEnum) -> Vec<TokenStream> {
    let mut last: Option<Expr> = None;
    let mut offset = 0i32;
    data.variants
        .iter()
        .map(|v| {
            if let Some((_, expr)) = &v.discriminant {
                last = Some(expr.clone());
                offset = 0;
            }
            let value = match &last {
                Some(expr) => quote! { ((#expr) as i32 + #offset) },
                None => quote! { #offset },
            };
            offset += 1;
            value
        })
        .collect()
}

fn encode_field(access: TokenStream, attrs: &[syn::Attribute]) -> TokenStream {
    if is_var(attrs) {
        quote! { ::rapidbot_buf::EncodeVar::encode_var(#access, buf); }
    } else {
        quote! { ::rapidbot_buf::Encode::encode(#access, buf); }
    }
}

fn decode_field(attrs: &[syn::Attribute]) -> TokenStream {
    if is_var(attrs) {
        quote! { ::rapidbot_buf::DecodeVar::decode_var(buf)? }
    } else {
        quote! { ::rapidbot_buf::Decode::decode(buf)? }
    }
}

/// Binding names for a variant's fields, and a pattern that binds them.
fn bindings(fields: &Fields) -> (Vec<Ident>, TokenStream) {
    match fields {
        Fields::Named(named) => {
            let names: Vec<_> = named.named.iter().map(|f| f.ident.clone().unwrap()).collect();
            let pat = quote! { { #(#names),* } };
            (names, pat)
        }
        Fields::Unnamed(unnamed) => {
            let names: Vec<_> = (0..unnamed.unnamed.len()).map(|i| format_ident!("f{i}")).collect();
            let pat = quote! { ( #(#names),* ) };
            (names, pat)
        }
        Fields::Unit => (Vec::new(), quote! {}),
    }
}

fn construct(fields: &Fields) -> TokenStream {
    match fields {
        Fields::Named(named) => {
            let inits = named.named.iter().map(|f| {
                let name = f.ident.as_ref().unwrap();
                let value = decode_field(&f.attrs);
                quote! { #name: #value }
            });
            quote! { { #(#inits),* } }
        }
        Fields::Unnamed(unnamed) => {
            let values = unnamed.unnamed.iter().map(|f| decode_field(&f.attrs));
            quote! { ( #(#values),* ) }
        }
        Fields::Unit => quote! {},
    }
}

fn expand_encode(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => {
            let (names, pat) = bindings(&data.fields);
            let writes = data
                .fields
                .iter()
                .zip(&names)
                .map(|(f, n)| encode_field(quote! { #n }, &f.attrs));
            quote! {
                let Self #pat = self;
                #(#writes)*
            }
        }
        Data::Enum(data) => {
            let disc_ty = discriminant_type(input)?;
            let arms = data.variants.iter().zip(discriminants(data)).map(|(v, disc)| {
                let ident = &v.ident;
                let (names, pat) = bindings(&v.fields);
                let write_disc = match &disc_ty {
                    Some(ty) => quote! { ::rapidbot_buf::Encode::encode(&((#disc) as #ty), buf); },
                    None => quote! { ::rapidbot_buf::EncodeVar::encode_var(&(#disc), buf); },
                };
                let writes = v.fields.iter().zip(&names).map(|(f, n)| encode_field(quote! { #n }, &f.attrs));
                quote! {
                    Self::#ident #pat => {
                        #write_disc
                        #(#writes)*
                    }
                }
            });
            quote! {
                match self {
                    #(#arms)*
                }
            }
        }
        Data::Union(_) => return Err(syn::Error::new_spanned(input, "unions are not supported")),
    };

    Ok(quote! {
        impl #impl_generics ::rapidbot_buf::Encode for #name #ty_generics #where_clause {
            #[allow(unused_variables)]
            fn encode(&self, buf: &mut ::std::vec::Vec<u8>) {
                #body
            }
        }
    })
}

fn expand_decode(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => {
            let ctor = construct(&data.fields);
            quote! { Ok(Self #ctor) }
        }
        Data::Enum(data) => {
            let disc_ty = discriminant_type(input)?;
            let read_disc = match &disc_ty {
                Some(ty) => quote! { <#ty as ::rapidbot_buf::Decode>::decode(buf)? as i32 },
                None => quote! { <i32 as ::rapidbot_buf::DecodeVar>::decode_var(buf)? },
            };
            let arms = data.variants.iter().zip(discriminants(data)).map(|(v, disc)| {
                let ident = &v.ident;
                let ctor = construct(&v.fields);
                quote! { d if d == #disc => Ok(Self::#ident #ctor), }
            });
            quote! {
                let d = #read_disc;
                match d {
                    #(#arms)*
                    value => Err(::rapidbot_buf::DecodeError::InvalidDiscriminant { ty: #name_str, value }),
                }
            }
        }
        Data::Union(_) => return Err(syn::Error::new_spanned(input, "unions are not supported")),
    };

    Ok(quote! {
        impl #impl_generics ::rapidbot_buf::Decode for #name #ty_generics #where_clause {
            fn decode(buf: &mut &[u8]) -> ::std::result::Result<Self, ::rapidbot_buf::DecodeError> {
                #body
            }
        }
    })
}
