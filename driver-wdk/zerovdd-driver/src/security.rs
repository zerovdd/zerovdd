//! Caller token verification for `\\.\ZeroVddControl` handles inside `WUDFHost.exe`.

use core::ptr;

use wdk_sys::windows::{
    AllocateAndInitializeSid, CheckTokenMembership, CloseHandle, FreeSid, GetCurrentThread,
    OpenThreadToken,
};
use wdk_sys::{
    BOOL, DOMAIN_ALIAS_RID_ADMINS, HANDLE, PSID, SECURITY_BUILTIN_DOMAIN_RID,
    SECURITY_LOCAL_SYSTEM_RID, SID_IDENTIFIER_AUTHORITY, TOKEN_QUERY,
};

const SECURITY_NT_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 5],
};

// SAFETY: Must only be called while impersonating a client thread inside `WdfRequestImpersonate`.
unsafe fn token_matches_sid(
    token: HANDLE,
    sub_auth_count: u8,
    sub_auth0: u32,
    sub_auth1: u32,
) -> bool {
    let mut authority = SECURITY_NT_AUTHORITY;
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: `authority` and `sid` are valid stack locals; `sid` is freed with `FreeSid` below.
    let alloc_ok = unsafe {
        AllocateAndInitializeSid(
            &mut authority,
            sub_auth_count,
            sub_auth0,
            sub_auth1,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut sid,
        )
    };
    if alloc_ok == 0 || sid.is_null() {
        return false;
    }

    let mut is_member: BOOL = 0;
    // SAFETY: `token` is a valid queryable impersonation token and `sid` was just initialized.
    let check_ok = unsafe { CheckTokenMembership(token, sid, &mut is_member) };
    // SAFETY: `sid` was allocated by `AllocateAndInitializeSid`.
    unsafe {
        let _ = FreeSid(sid);
    }
    check_ok != 0 && is_member != 0
}

/// Returns `true` when the currently impersonated caller holds `SYSTEM` or `Administrators`.
///
/// # Safety
///
/// Caller must invoke this inside `EVT_WDF_REQUEST_IMPERSONATE` so the current thread carries
/// the client's impersonation token.
// SAFETY: Caller must invoke this inside `EVT_WDF_REQUEST_IMPERSONATE` so the current thread
// carries the client's impersonation token.
pub unsafe fn is_impersonated_caller_system_or_admin() -> bool {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentThread` returns a pseudo-handle; `OpenAsSelf = 1` checks access
    // against `WUDFHost.exe`'s process context while still opening the thread's
    // `SecurityIdentification` client impersonation token (never `WUDFHost.exe`'s primary token).
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if opened == 0 || token.is_null() {
        return false;
    }

    // SAFETY: `token` is an open impersonation token with `TOKEN_QUERY` access.
    let is_system = unsafe { token_matches_sid(token, 1, SECURITY_LOCAL_SYSTEM_RID, 0) };
    // SAFETY: `token` remains open and valid for the second membership check.
    let is_admin = unsafe {
        token_matches_sid(
            token,
            2,
            SECURITY_BUILTIN_DOMAIN_RID,
            DOMAIN_ALIAS_RID_ADMINS,
        )
    };

    // SAFETY: `token` was opened by `OpenThreadToken` and must be closed exactly once.
    unsafe {
        let _ = CloseHandle(token);
    }
    is_system || is_admin
}
