//! Regression test for results/audit/HONESTY_AUDIT.md finding #21.
//!
//! `IUserManager.isUserUnlocked` (and the other `is*` queries) must not answer
//! `true` for an unknown user id: ACOS has exactly one user (id 0).

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_services::declared::{UserService, UserState};
use acos_services::host::ServiceObject;
use acos_services::value::Value;

const IFACE: &str = "android.os.IUserManager";

fn method(name: &str) -> MethodInfo {
    MethodInfo {
        name: name.to_string(),
        signature: String::new(),
        return_type: "Z".to_string(),
        code: 1,
        oneway: false,
        args: Vec::new(),
        reply: Vec::new(),
        incomplete: None,
    }
}

#[test]
fn unknown_user_is_not_reported_unlocked() {
    let mut svc = UserService::new(Arc::new(Mutex::new(UserState::new())));
    let out = svc
        .call(IFACE, &method("isUserUnlocked"), &[Value::Int(99)])
        .expect("isUserUnlocked must answer");
    assert_ne!(
        out.reply.first(),
        Some(&Value::Bool(true)),
        "isUserUnlocked(99) returned true for a user that does not exist"
    );

    let out0 = svc
        .call(IFACE, &method("isUserUnlocked"), &[Value::Int(0)])
        .expect("isUserUnlocked must answer");
    assert_eq!(out0.reply.first(), Some(&Value::Bool(true)));
}
