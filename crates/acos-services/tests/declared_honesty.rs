//! Regression test for results/audit/HONESTY_AUDIT.md finding #4.
//!
//! `IAppOpsService.checkOperation` must not return MODE_ALLOWED for a
//! permission-gated op the caller does not hold. Android returns
//! MODE_ERRORED / MODE_IGNORED for a restricted op without the permission.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_services::declared::{AppOpsService, AppOpsState, MODE_ALLOWED};
use acos_services::host::ServiceObject;
use acos_services::value::Value;

const IFACE: &str = "com.android.internal.app.IAppOpsService";

fn method(name: &str) -> MethodInfo {
    MethodInfo {
        name: name.to_string(),
        signature: String::new(),
        return_type: "I".to_string(),
        code: 1,
        oneway: false,
        args: Vec::new(),
        reply: Vec::new(),
        incomplete: None,
    }
}

#[test]
fn restricted_op_without_a_grant_is_not_mode_allowed() {
    let mut svc = AppOpsService::new(Arc::new(Mutex::new(AppOpsState::new())));
    // OP_CAMERA = 26. No camera permission was granted to this uid/package.
    let args = [
        Value::Int(26),
        Value::Int(10_192),
        Value::Str(Some("dev.acos.fixtures.helloviews".to_string())),
    ];
    let out = svc
        .call(IFACE, &method("checkOperation"), &args)
        .expect("checkOperation must answer");
    assert_ne!(
        out.reply.first(),
        Some(&Value::Int(MODE_ALLOWED)),
        "checkOperation(OP_CAMERA) returned MODE_ALLOWED without a grant"
    );
}

#[test]
fn restricted_op_with_grant_is_mode_allowed_again() {
    let state = Arc::new(Mutex::new(AppOpsState::new()));
    let mut svc = AppOpsService::new(state);
    svc.grant_permission("android.permission.CAMERA");
    assert_eq!(
        svc.granted_permissions(),
        vec!["android.permission.CAMERA".to_string()]
    );
    let args = [Value::Int(26), Value::Int(10_192), Value::Str(None)];
    let out = svc
        .call(IFACE, &method("checkOperation"), &args)
        .expect("checkOperation must answer");
    assert_eq!(out.reply.first(), Some(&Value::Int(MODE_ALLOWED)));
}
