use std::ffi::CStr;
use std::ffi::c_uint;

use clang_sys::{
    clang_disposeString, clang_disposeTokens, clang_getCString, clang_getCursorExtent,
    clang_getFileLocation, clang_getRangeEnd, clang_getRangeStart, clang_getTokenExtent,
    clang_getTokenSpelling, clang_tokenize, CXCursor, CXSourceLocation, CXString, CXToken,
    CXTranslationUnit,
};
use tower_lsp::lsp_types::{Position, Range};

pub(crate) unsafe fn tokenize_cursor(
    tu: CXTranslationUnit,
    cursor: CXCursor,
) -> Option<Vec<CXToken>> {
    let range = clang_getCursorExtent(cursor);
    let mut tokens_ptr: *mut CXToken = std::ptr::null_mut();
    let mut length: c_uint = 0;
    clang_tokenize(tu, range, &mut tokens_ptr, &mut length);
    if tokens_ptr.is_null() || length == 0 {
        return None;
    }
    let tokens = std::slice::from_raw_parts(tokens_ptr, length as usize).to_vec();
    clang_disposeTokens(tu, tokens_ptr, length);
    Some(tokens)
}

pub(crate) unsafe fn split_macro_args(
    tu: CXTranslationUnit,
    tokens: &[CXToken],
) -> Option<Vec<Vec<CXToken>>> {
    let mut args = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0;
    let mut collecting = false;
    for token in tokens {
        let spelling = tokens_to_string(tu, &[*token]).unwrap_or_default();
        match spelling.as_str() {
            "(" if !collecting => collecting = true,
            "(" => {
                depth += 1;
                current.push(*token);
            }
            ")" if depth == 0 => {
                args.push(current.clone());
                break;
            }
            ")" => {
                depth -= 1;
                current.push(*token);
            }
            "," if depth == 0 => {
                args.push(current.clone());
                current.clear();
            }
            _ if collecting => current.push(*token),
            _ => {}
        }
    }
    Some(args)
}

pub(crate) unsafe fn tokens_to_string(
    tu: CXTranslationUnit,
    tokens: &[CXToken],
) -> Option<String> {
    let mut buffer = String::new();
    for token in tokens {
        let spelling = clang_getTokenSpelling(tu, *token);
        let text = cxstring_to_string(spelling);
        buffer.push_str(&text);
    }
    Some(buffer)
}

pub(crate) unsafe fn tokens_range(
    tu: CXTranslationUnit,
    tokens: &[CXToken],
) -> Option<Range> {
    let first = tokens.first()?;
    let last = tokens.last()?;
    let start = token_range(tu, *first)?.start;
    let end = token_range(tu, *last)?.end;
    Some(Range { start, end })
}

pub(crate) unsafe fn token_range(tu: CXTranslationUnit, token: CXToken) -> Option<Range> {
    let extent = clang_getTokenExtent(tu, token);
    Some(Range {
        start: cxlocation_to_position(clang_getRangeStart(extent))?,
        end: cxlocation_to_position(clang_getRangeEnd(extent))?,
    })
}

pub(crate) unsafe fn cursor_range(cursor: CXCursor) -> Option<Range> {
    let extent = clang_getCursorExtent(cursor);
    Some(Range {
        start: cxlocation_to_position(clang_getRangeStart(extent))?,
        end: cxlocation_to_position(clang_getRangeEnd(extent))?,
    })
}

pub(crate) unsafe fn cxlocation_to_position(location: CXSourceLocation) -> Option<Position> {
    let mut line = 0;
    let mut column = 0;
    let mut offset = 0;
    clang_getFileLocation(
        location,
        std::ptr::null_mut(),
        &mut line,
        &mut column,
        &mut offset,
    );
    Some(Position::new(
        line.saturating_sub(1),
        column.saturating_sub(1),
    ))
}

pub(crate) unsafe fn cxstring_to_string(s: CXString) -> String {
    let c_str = clang_getCString(s);
    let result = if c_str.is_null() {
        String::new()
    } else {
        CStr::from_ptr(c_str).to_string_lossy().into_owned()
    };
    clang_disposeString(s);
    result
}
