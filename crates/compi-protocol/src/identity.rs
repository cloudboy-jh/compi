use crate::{Error, Result};
use std::ffi::OsStr;
use std::iter::once;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

const SDDL_REVISION_1: u32 = 1;

#[derive(Debug, Clone)]
pub struct InstanceNames {
    pub pipe: String,
    pub mutex: String,
}

pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    pub fn for_current_user() -> Result<Self> {
        let sid = current_user_sid_string()?;
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    pub fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_invalid() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
            }
        }
    }
}

pub fn instance_names(instance: Option<&str>) -> Result<InstanceNames> {
    let sid = current_user_sid_string()?;
    let suffix = match instance {
        Some(value) => format!("-{}", validate_instance(value)?),
        None => String::new(),
    };
    Ok(InstanceNames {
        pipe: format!(r"\\.\pipe\compi-daemon-{sid}{suffix}"),
        mutex: format!(r"Local\CompiDaemon-{sid}{suffix}"),
    })
}

pub fn current_user_sid_string() -> Result<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = OwnedHandle::from_raw_handle(token.0);

        let mut required = 0_u32;
        let _ = GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            None,
            0,
            &mut required,
        );
        if required == 0 {
            return Err(windows::core::Error::from_thread().into());
        }

        let word_size = size_of::<usize>();
        let mut storage = vec![0_usize; (required as usize).div_ceil(word_size)];
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            required,
            &mut required,
        )?;
        let token_user = &*storage.as_ptr().cast::<TOKEN_USER>();

        let mut string_sid = PWSTR::null();
        ConvertSidToStringSidW(token_user.User.Sid, &mut string_sid)?;
        let result = string_sid.to_string().map_err(Error::from);
        let _ = LocalFree(Some(HLOCAL(string_sid.0.cast())));
        result
    }
}

fn validate_instance(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("instance names must be 1-32 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(value)
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

use std::os::windows::io::AsRawHandle;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_are_stable_and_scoped() {
        let first = instance_names(Some("test_1")).unwrap();
        let second = instance_names(Some("test_1")).unwrap();

        assert_eq!(first.pipe, second.pipe);
        assert!(first.pipe.ends_with("-test_1"));
        assert!(first.mutex.ends_with("-test_1"));
    }

    #[test]
    fn rejects_unsafe_instance_names() {
        assert!(instance_names(Some("../other")).is_err());
        assert!(instance_names(Some("")).is_err());
    }
}
