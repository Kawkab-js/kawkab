#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimePolicy {
    pub allow_child_process: bool,
}

impl RuntimePolicy {
    pub(crate) fn from_env() -> Self {
        let allow = std::env::var("KAWKAB_ALLOW_CHILD_PROCESS")
            .map(|v| parse_allow_flag(&v))
            .unwrap_or(false);
        Self {
            allow_child_process: allow,
        }
    }
}

fn parse_allow_flag(v: &str) -> bool {
    matches!(v, "1" | "true" | "TRUE" | "yes" | "YES")
}
