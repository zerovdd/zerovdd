use syn::meta::ParseNestedMeta;
use syn::{Attribute, Expr};

fn consume_nested_meta(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        let _ = meta.value()?.parse::<Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        meta.parse_nested_meta(|inner| consume_nested_meta(&inner))?;
    }
    Ok(())
}

fn eval_cfg_meta(meta: &ParseNestedMeta<'_>, allow_any: bool) -> syn::Result<bool> {
    if meta.path.is_ident("test")
        && !meta.input.peek(syn::Token![=])
        && !meta.input.peek(syn::token::Paren)
    {
        return Ok(true);
    }

    if meta.path.is_ident("all") && meta.input.peek(syn::token::Paren) {
        let mut found = false;
        meta.parse_nested_meta(|inner| {
            if eval_cfg_meta(&inner, allow_any)? {
                found = true;
            }
            Ok(())
        })?;
        return Ok(found);
    }

    if meta.path.is_ident("any") && meta.input.peek(syn::token::Paren) {
        if allow_any {
            let mut found = false;
            meta.parse_nested_meta(|inner| {
                if eval_cfg_meta(&inner, true)? {
                    found = true;
                }
                Ok(())
            })?;
            return Ok(found);
        }
        consume_nested_meta(meta)?;
        return Ok(false);
    }

    // WHY: Explicitly consume `not(...)` and key-value predicates (`feature = "..."`,
    // `target_os = "..."`) without matching `not(test)` so sibling predicates in `all(...)`
    // are always visited.
    consume_nested_meta(meta)?;
    Ok(false)
}

pub fn cfg_is_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if eval_cfg_meta(&meta, false)? {
                found = true;
            }
            Ok(())
        });
        found
    })
}

pub fn cfg_mentions_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if eval_cfg_meta(&meta, true)? {
                found = true;
            }
            Ok(())
        });
        found
    })
}

pub fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || attr
                .path()
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "test")
    })
}
