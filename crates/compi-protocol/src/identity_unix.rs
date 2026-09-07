use crate::Result;

#[derive(Debug, Clone)]
pub struct InstanceNames {
    pub pipe: String,
    pub mutex: String,
}

pub fn instance_names(instance: Option<&str>) -> Result<InstanceNames> {
    let name = instance.unwrap_or("default");
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("instance names must be 1-32 ASCII letters, digits, '-' or '_'".into());
    }
    let directory = crate::paths::runtime_dir()?;
    let pipe = directory.join(format!("{name}.sock"));
    let mutex = directory.join(format!("{name}.lock"));
    Ok(InstanceNames {
        pipe: pipe.to_str().ok_or("socket path must be UTF-8")?.to_owned(),
        mutex: mutex.to_str().ok_or("lock path must be UTF-8")?.to_owned(),
    })
}
