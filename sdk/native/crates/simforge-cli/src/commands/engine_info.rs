pub fn package_version() -> String {
    let meta = env!("SIMFORGE_BUILD_SEMVER_META");
    if meta.is_empty() {
        env!("CARGO_PKG_VERSION").to_owned()
    } else {
        format!("{}+{meta}", env!("CARGO_PKG_VERSION"))
    }
}

pub fn engine_sem_ver() -> serde_json::Value {
    serde_json::Value::Null
}

pub const COMMIT: &str = env!("SIMFORGE_BUILD_SOURCE_REVISION");
