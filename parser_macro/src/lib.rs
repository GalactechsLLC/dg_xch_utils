use bytes::Buf;
use dg_xch_serialize::{CONS_BOX_MARKER, MAX_SINGLE_BYTE, decode_size};
use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf, PrefixComponent};
use syn::{
    Error, Expr, Ident, LitStr, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    spanned::Spanned,
};

struct Args {
    base: Ident,
    _comma: Token![,],
    hex_expr: Expr,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            base: input.parse()?,     // e.g., CAT2
            _comma: input.parse()?,   // ,
            hex_expr: input.parse()?, // "..."
        })
    }
}

#[proc_macro]
pub fn parse_program_hex(input: TokenStream) -> TokenStream {
    let Args { base, hex_expr, .. } = syn::parse_macro_input!(input as Args);

    let input_kind = match eval_expr(&hex_expr) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    let (hex_str, bytes) = match input_kind {
        InputKind::RawHex(s) => {
            let b = match decode_hex(s.trim()) {
                Ok(b) => b,
                Err(e) => return compile_error(&e),
            };
            (s, b)
        }
        InputKind::Path(p) => {
            let path = {
                let pth = Path::new(&p);
                if pth.is_absolute() {
                    pth.to_path_buf()
                } else {
                    let manifest =
                        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
                    join_lex_norm(Path::new(&manifest), &p)
                }
            };
            let contents = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return compile_error(&format!(
                        "Failed to read hex file {}: {}",
                        path.display(),
                        e
                    ));
                }
            };
            // Puzzle source files may contain whitespace and comments around the hex payload.
            let cleaned: String = contents.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            if !cleaned.len().is_multiple_of(2) {
                return compile_error("hex file has odd number of hex digits after cleanup");
            }
            let b = match decode_hex(&cleaned) {
                Ok(b) => b,
                Err(e) => return compile_error(&e),
            };
            (cleaned, b)
        }
    };

    let mut cursor = Cursor::new(bytes.as_slice());
    let dag = match parse_clvm(&mut cursor) {
        Ok(d) => d,
        Err(e) => return compile_error(&e),
    };
    let order = topo_order(&dag);
    let ts = codegen(&base, hex_str.trim(), &bytes, &dag, &order);
    ts.into()
}

enum InputKind {
    RawHex(String),
    Path(String),
}

fn eval_expr(expr: &Expr) -> Result<InputKind, Error> {
    match expr {
        Expr::Macro(m) if m.mac.path.is_ident("concat") => {
            let args: Punctuated<Expr, Token![,]> = m
                .mac
                .parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated)?;

            let mut out = String::new();
            for (i, e) in args.iter().enumerate() {
                match e {
                    Expr::Macro(mm) if mm.mac.path.is_ident("env") => {
                        if i != 0 {
                            return Err(Error::new(mm.span(), r#"env!("OUT_DIR") must be first"#));
                        }
                        //parse env! body as a LitStr
                        let var: LitStr = mm.mac.parse_body()?;
                        if var.value() != "OUT_DIR" {
                            return Err(Error::new(
                                var.span(),
                                r#"only env!("OUT_DIR") is supported"#,
                            ));
                        }
                        let base = std::env::var("CALLER_OUT_DIR")
                            .or_else(|_| std::env::var("OUT_DIR"))
                            .map_err(|_| Error::new(mm.span(), "OUT_DIR not set"))?;
                        out.push_str(&base);
                    }
                    Expr::Lit(l2) => {
                        if let syn::Lit::Str(s) = &l2.lit {
                            out.push_str(&s.value());
                        } else {
                            return Err(Error::new(
                                l2.span(),
                                "concat! pieces must be string literals or env!(\"OUT_DIR\")",
                            ));
                        }
                    }
                    Expr::Group(g) => match eval_expr(&g.expr)? {
                        InputKind::RawHex(s) | InputKind::Path(s) => out.push_str(&s),
                    },
                    _ => return Err(Error::new(e.span(), "unsupported concat! piece")),
                }
            }
            Ok(InputKind::Path(out))
        }

        Expr::Lit(l) => {
            if let syn::Lit::Str(s) = &l.lit {
                let v = s.value();
                if looks_like_path(&v) {
                    Ok(InputKind::Path(v))
                } else if is_probable_hex(&v) {
                    Ok(InputKind::RawHex(v))
                } else {
                    Err(Error::new(
                        s.span(),
                        r#"expected raw hex (even-length hex string) or a .hex file path"#,
                    ))
                }
            } else {
                Err(Error::new(l.lit.span(), "expected string literal"))
            }
        }

        Expr::Group(g) => eval_expr(&g.expr),
        other => Err(Error::new(other.span(), "unsupported expression")),
    }
}

fn is_probable_hex(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.len().is_multiple_of(2) && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn looks_like_path(s: &str) -> bool {
    let t = s.trim();
    // quick wins
    if t.ends_with(".hex") || t.starts_with("./") || t.starts_with("../") || t.starts_with("~/") {
        return true;
    }
    // absolute *nix
    if t.starts_with(std::path::MAIN_SEPARATOR) {
        return true;
    }
    // absolute Windows "C:\..."
    if t.len() >= 3
        && t.as_bytes()[1] == b':'
        && (t.as_bytes()[2] == b'\\' || t.as_bytes()[2] == b'/')
    {
        return true;
    }
    // any path separator
    if t.contains('/') || t.contains('\\') {
        return true;
    }
    false
}

fn join_lex_norm(base: &Path, tail: &str) -> PathBuf {
    let joined = base.join(tail);

    let mut prefix: Option<PrefixComponent> = None;
    let mut has_root = false;
    let mut stack: Vec<std::ffi::OsString> = Vec::new();

    for comp in joined.components() {
        match comp {
            Component::Prefix(p) => {
                prefix = Some(p);
            }
            Component::RootDir => {
                has_root = true;
                stack.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = stack.pop();
            }
            Component::Normal(n) => stack.push(n.to_os_string()),
        }
    }

    let mut out = PathBuf::new();
    if let Some(p) = prefix {
        out.push(p.as_os_str());
    }
    if has_root {
        out.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for seg in stack {
        out.push(seg);
    }
    out
}

fn resolve_crate_path(wanted: &str) -> TokenStream2 {
    match crate_name(wanted) {
        Ok(FoundCrate::Itself) => {
            let ident = format_ident!("crate");
            quote!(#ident)
        }
        Ok(FoundCrate::Name(actual)) => {
            let ident = format_ident!("{}", actual);
            quote!(::#ident)
        }
        Err(_) => {
            let ident = format_ident!("{}", wanted);
            quote!(::#ident)
        }
    }
}

fn compile_error(msg: &str) -> TokenStream {
    let ts: TokenStream2 = quote! { compile_error!(#msg); };
    ts.into()
}

#[derive(Clone, Debug)]
enum MNode {
    Null,
    Atom { i: usize, l: usize },
    Pair { l: usize, r: usize },
}

#[derive(Clone, Debug)]
struct MDag {
    nodes: Vec<MNode>,
    root: usize,
}

fn parse_clvm<T: AsRef<[u8]>>(stream: &mut Cursor<T>) -> Result<MDag, String> {
    if !stream.has_remaining() {
        return Ok(MDag {
            nodes: vec![MNode::Null],
            root: 0,
        });
    }
    #[derive(Copy, Clone)]
    enum Op {
        Exp,
        Cons,
    }
    let mut nodes: Vec<MNode> = Vec::with_capacity(1024);
    nodes.push(MNode::Null);
    let mut byte_buf = [0; 1];
    let mut ops: Vec<Op> = vec![Op::Exp];
    let mut vals: Vec<usize> = Vec::with_capacity(1024);
    while let Some(op) = ops.pop() {
        match op {
            Op::Exp => {
                ensure(
                    stream.has_remaining(),
                    "Invalid SExp: Unexpected end of stream",
                )?;
                stream
                    .read_exact(&mut byte_buf)
                    .map_err(|e| format!("{e:?}"))?;
                if byte_buf[0] == CONS_BOX_MARKER {
                    ops.push(Op::Cons);
                    ops.push(Op::Exp);
                    ops.push(Op::Exp);
                } else if byte_buf[0] == 0x80 {
                    vals.push(0);
                } else if byte_buf[0] <= MAX_SINGLE_BYTE {
                    let i = stream.position() as usize - 1;
                    let l = 1;
                    let this = nodes.len();
                    nodes.push(MNode::Atom { i, l });
                    vals.push(this);
                } else {
                    let blob_size =
                        decode_size(stream, byte_buf[0]).map_err(|e| e.to_string())? as usize;
                    if stream.remaining() < blob_size {
                        return Err(format!(
                            "Bad encoding: Index is {}, Length is {}, Remaining is {}",
                            stream.position(),
                            blob_size,
                            stream.remaining()
                        ));
                    }
                    let this = nodes.len();
                    nodes.push(MNode::Atom {
                        i: stream.position() as usize,
                        l: blob_size,
                    });
                    stream
                        .seek(SeekFrom::Current(blob_size as i64))
                        .map_err(|e| format!("{e:?}"))?;
                    vals.push(this);
                }
            }
            Op::Cons => {
                ensure(
                    vals.len() >= 2,
                    "Bad encoding: Cons with fewer than 2 values",
                )?;
                let r = vals.pop().unwrap();
                let l = vals.pop().unwrap();
                let this = nodes.len();
                nodes.push(MNode::Pair { l, r });
                vals.push(this);
            }
        }
    }
    ensure(vals.len() == 1, "Invalid SExp: stack not singular at end")?;
    let root = vals.pop().unwrap();
    Ok(MDag { nodes, root })
}

fn ensure(cond: bool, msg: &'static str) -> Result<(), String> {
    if cond { Ok(()) } else { Err(msg.into()) }
}

fn topo_order(dag: &MDag) -> Vec<usize> {
    let mut out = Vec::with_capacity(dag.nodes.len());
    let mut seen = vec![false; dag.nodes.len()];
    fn dfs(i: usize, dag: &MDag, seen: &mut [bool], out: &mut Vec<usize>) {
        if seen[i] {
            return;
        }
        seen[i] = true;
        match dag.nodes[i] {
            MNode::Null => {}
            MNode::Atom { .. } => {}
            MNode::Pair { l, r } => {
                dfs(l, dag, seen, out);
                dfs(r, dag, seen, out);
            }
        }
        out.push(i);
    }
    dfs(dag.root, dag, &mut seen, &mut out);
    out
}

fn codegen(base: &Ident, hex: &str, bytes: &[u8], dag: &MDag, order: &[usize]) -> TokenStream2 {
    // create a stable private module name using a simple hash of bytes
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let hash = h.finish();
    let mod_ident = format_ident!("__clvm_prog_{hash:016x}");
    let n = bytes.len();
    let byte_literals = bytes.iter().map(|b| quote! { #b });
    // map node index -> static ident
    let mut idents = vec![format_ident!("N0"); dag.nodes.len()];
    for (slot, idx) in order.iter().enumerate() {
        // give deterministic names N1, N2... for nodes in topo order (N0 is reserved Null)
        let name = format_ident!("N{}", slot + 1);
        idents[*idx] = name;
    }
    // build `const` items for each node in topo order
    let mut node_statics: Vec<TokenStream2> = Vec::with_capacity(order.len());
    let core = resolve_crate_path("dg_xch_core");
    for &idx in order {
        let ident = &idents[idx];
        match dag.nodes[idx] {
            MNode::Null => {
                node_statics.push(quote! {
                    pub const #ident: #core::clvm::sexp::SExp = #core::constants::NULL_SEXP;
                });
            }
            MNode::Atom { i, l } => {
                if l == 0 {
                    node_statics.push(quote! {
                        pub const #ident: #core::clvm::sexp::SExp = #core::constants::NULL_SEXP;
                    });
                } else {
                    let start = i;
                    let end = i + l;
                    let elems = bytes[start..end].iter().map(|b| quote! { #b });
                    node_statics.push(quote! {
                    pub const #ident: #core::clvm::sexp::SExp =
                        #core::clvm::sexp::SExp::Atom(
                            #core::clvm::sexp::AtomBuf::Borrowed(&[#( #elems ),* ])
                        );
                    });
                }
            }
            MNode::Pair { l, r } => {
                let l_ident = &idents[l];
                let r_ident = &idents[r];
                node_statics.push(quote! {
                    pub const #ident: #core::clvm::sexp::SExp =
                        #core::clvm::sexp::SExp::Pair(
                            #core::clvm::sexp::PairBuf::Borrowed((&#l_ident, &#r_ident))
                        );
                });
            }
        }
    }
    let root_ident = &idents[dag.root];
    let ident_src = format_ident!("{}_SRC", base);
    let ident_sexp = format_ident!("{}_SEXP", base);
    let ident_program = format_ident!("{}_PROGRAM", base);
    let ident_hex = format_ident!("{}_HEX", base);
    let ident_tree_hash = format_ident!("{}_TREE_HASH", base);
    let tree_hash = compute_tree_hash(bytes, dag);
    let tree_hash_bytes = tree_hash.iter().map(|b| quote! { #b });
    quote! {
        mod #mod_ident {
            pub static __ARR: [u8; #n] = [ #( #byte_literals ),* ];
            #(#node_statics)*
            pub static ROOT: &#core::clvm::sexp::SExp = &#root_ident;
        }
        pub static #ident_src: &'static [u8] = &#mod_ident::__ARR;
        pub static #ident_sexp: &'static #core::clvm::sexp::SExp = &#mod_ident::ROOT;
        pub static #ident_program: #core::clvm::program::Program = #core::clvm::program::Program::new_static(#mod_ident::ROOT);
        pub static #ident_hex: &str = #hex;
        pub const #ident_tree_hash: #core::blockchain::sized_bytes::Bytes32 =
            #core::blockchain::sized_bytes::Bytes32::const_new([ #( #tree_hash_bytes ),* ]);
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let mut nibbles = Vec::with_capacity(s.len());
    for &b in s.as_bytes() {
        if let Some(v) = from_hex(b) {
            nibbles.push(v);
        }
    }

    if nibbles.len() % 2 != 0 {
        return Err("Odd number of hex digits".into());
    }

    let mut out = Vec::with_capacity(nibbles.len() / 2);
    for i in (0..nibbles.len()).step_by(2) {
        out.push((nibbles[i] << 4) | nibbles[i + 1]);
    }
    Ok(out)
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        b'A'..=b'F' => Some(10 + (b - b'A')),
        _ => None,
    }
}

fn th_atom(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(data);
    hasher.finalize().into()
}

fn th_pair(l: [u8; 32], r: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([0x02]);
    hasher.update(l);
    hasher.update(r);
    hasher.finalize().into()
}

fn compute_tree_hash(bytes: &[u8], dag: &MDag) -> [u8; 32] {
    // Post-order over the same topo order you already build
    fn go(idx: usize, dag: &MDag, bytes: &[u8], memo: &mut Vec<Option<[u8; 32]>>) -> [u8; 32] {
        if let Some(h) = memo[idx] {
            return h;
        }
        let h = match dag.nodes[idx] {
            MNode::Null => th_atom(&[]),
            MNode::Atom { i, l } => th_atom(&bytes[i..i + l]),
            MNode::Pair { l, r } => {
                let lh = go(l, dag, bytes, memo);
                let rh = go(r, dag, bytes, memo);
                th_pair(lh, rh)
            }
        };
        memo[idx] = Some(h);
        h
    }
    let mut memo = vec![None; dag.nodes.len()];
    go(dag.root, dag, bytes, &mut memo)
}
