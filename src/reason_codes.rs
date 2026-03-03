pub const REASON_INJECTION_BLOCKED: &str = "injection_blocked";
pub const REASON_LOOP_GUARD_TRIGGERED: &str = "loop_guard_triggered";
pub const REASON_CI_GATE_BLOCKED: &str = "ci_gate_blocked";
pub const REASON_ROLLBACK_TRIGGERED: &str = "rollback_triggered";
pub const REASON_SELF_DEV_MODE_RESTRICTION: &str = "self_dev_mode_restriction";

pub fn reason_tag(reason: &str) -> String {
    format!("[reason:{}]", reason)
}

pub fn with_reason(reason: &str, message: impl AsRef<str>) -> String {
    format!("{} {}", reason_tag(reason), message.as_ref())
}
