//! `PendingIntent` senders — the system side (MODNOTIF4 item 2).
//!
//! On Android a `PendingIntent` *is* an `IIntentSender` binder the system
//! service handed out: `PendingIntent.writeToParcel` writes only that strong
//! binder (`base/core/java/android/app/PendingIntent.java:1500`), and firing it
//! means calling `IIntentSender.send`. So an app can only create the content
//! intent of a notification when `IActivityManager.getIntentSender` answers a
//! real sender binder — which is what this module builds.
//!
//! ## What is modelled
//!
//! * [`SenderTable`] is the `mIntentSenderRecords` map of
//!   `ActivityManagerService`: one [`PendingIntentRecord`] per live sender, keyed
//!   the way `PendingIntentRecord.Key.equals` keys it — `(type, package,
//!   requestCode, filter-equal intents, resolvedTypes, flags-without-the-three-
//!   ephemeral-bits)` (`PendingIntentRecord.java:140-190`; `Intent.filterEquals`
//!   ignores extras, so the key comparison does too).
//! * `getIntentSender` semantics (`ActivityManagerService.getIntentSender`,
//!   `base/services/core/java/com/android/server/am/ActivityManagerService.java`):
//!   `FLAG_NO_CREATE` answers the existing record or `null`, never creating one;
//!   `FLAG_CANCEL_CURRENT` cancels the matching record and creates a new one;
//!   `FLAG_UPDATE_CURRENT` keeps the record but replaces its stored extras;
//!   without either flag no record is created when one already matches.
//! * `send` replays the record's intents through the paths ACOS already owns:
//!   `start_activity` for a `getActivity` sender, the broadcast delivery queue
//!   for `getBroadcast`, `start_service` for `getService`. `FLAG_ONE_SHOT`
//!   cancels the record after the first send and a cancelled record refuses
//!   further sends — both are recorded as evidence, never silently ignored.
//! * `cancelIntentSender` (code 64) cancels the record; the node stays
//!   registered (its binder is a pool slot), so a later `send` is refused with
//!   the same cancelled status AOSP reports.
//!
//! ## The fill-in intent (PIFILLIN)
//!
//! `PendingIntent.send(context, code, intent)` hands `IIntentSender.send` a
//! **fill-in** intent, and the image's `PendingIntentRecord.sendInner` copies
//! the stored intent and merges the fill-in into the copy (the dex is
//! disassembled in `results/handoff/PIFULLIN/pending-intent-dex.txt`):
//!
//! * **mutable sender** (no `FLAG_IMMUTABLE`): `finalIntent.fillIn(intent,
//!   key.flags)` — the merge is [`crate::fill_in`], and the merged intent is
//!   what gets delivered (activity / broadcast / service).
//! * **immutable sender**: the framework skips the merge entirely, so the
//!   stored intent is delivered and the evidence records
//!   `fill_in_ignored: true`.

use std::collections::BTreeMap;

use acos_parcel::BinderId;

use crate::ams::{Extra, IntentFacts};
use crate::error::{Result, ServiceError};
use crate::value::Bundle;

/// `PendingIntent.FLAG_ONE_SHOT`.
pub const FLAG_ONE_SHOT: i32 = 0x4000_0000;
/// `PendingIntent.FLAG_NO_CREATE`.
pub const FLAG_NO_CREATE: i32 = 0x2000_0000;
/// `PendingIntent.FLAG_CANCEL_CURRENT`.
pub const FLAG_CANCEL_CURRENT: i32 = 0x1000_0000;
/// `PendingIntent.FLAG_UPDATE_CURRENT`.
pub const FLAG_UPDATE_CURRENT: i32 = 0x0800_0000;
/// `PendingIntent.FLAG_IMMUTABLE`.
pub const FLAG_IMMUTABLE: i32 = 0x0400_0000;
/// `PendingIntent.FLAG_MUTABLE`.
pub const FLAG_MUTABLE: i32 = 0x0200_0000;

/// The three flags `getIntentSender` strips before keying the record
/// (`ActivityManagerService.getIntentSender`).
pub const EPHEMERAL_FLAGS: i32 = FLAG_NO_CREATE | FLAG_CANCEL_CURRENT | FLAG_UPDATE_CURRENT;

/// `IActivityManager.getIntentSender` `type`: `INTENT_SENDER_ACTIVITY`.
pub const TYPE_ACTIVITY: i32 = 1;
/// `INTENT_SENDER_BROADCAST`.
pub const TYPE_BROADCAST: i32 = 2;
/// `INTENT_SENDER_SERVICE`.
pub const TYPE_SERVICE: i32 = 3;
/// `INTENT_SENDER_FOREGROUND_SERVICE`.
pub const TYPE_FOREGROUND_SERVICE: i32 = 4;

/// `android.content.IIntentSender` transaction code of `send`
/// (`IIntentSender.aidl` has exactly one method).
pub const TRANSACTION_SEND: u32 = 1;

/// The fill-in `Intent` of one `IIntentSender.send`, as far as the sender can
/// use it.
///
/// The system side decides between merging and ignoring before it looks at the
/// body: `PendingIntentRecord.sendInner` skips `Intent.fillIn` entirely for a
/// `FLAG_IMMUTABLE` sender. That distinction is kept here so an immutable
/// sender's fill-in is neither decoded (a body this codec cannot read must not
/// fail a send the framework performs) nor merged, and the evidence still says
/// a fill-in was present.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillIn<'a> {
    /// The caller sent no fill-in (`PendingIntent.send(context, code)`).
    None,
    /// A mutable sender received this fill-in: merge it.
    Intent(&'a IntentFacts),
    /// A fill-in arrived for an immutable sender: the framework ignores it.
    Ignored,
}

impl<'a> FillIn<'a> {
    /// Whether the caller sent a fill-in at all.
    #[must_use]
    pub fn is_some(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The fill-in body, when it is one the sender may merge.
    #[must_use]
    pub fn intent(self) -> Option<&'a IntentFacts> {
        match self {
            Self::Intent(intent) => Some(intent),
            Self::None | Self::Ignored => None,
        }
    }
}

/// One `send` (or one refused `send`).
#[derive(Debug, Clone, PartialEq)]
pub struct SendEvidence {
    /// The `code` argument of `send` (`Activity.RESULT_*` when the sender
    /// belongs to an activity result).
    pub code: i32,
    /// `Ok(code)` for the `start_activity`/`start_service` answer, or the
    /// structured error the delivery path raised.
    pub outcome: std::result::Result<i32, String>,
    /// The intent that was replayed.
    pub intent: IntentFacts,
    /// `true` when the record was cancelled (AOSP logs and returns).
    pub refused_cancelled: bool,
    /// `true` when the send carried a non-null fill-in `Intent` and the sender
    /// is immutable: the image's `sendInner` skips `Intent.fillIn` for
    /// `FLAG_IMMUTABLE`, so the stored intent is delivered unchanged.
    pub fill_in_ignored: bool,
    /// The caller's fill-in intent, exactly as it was read off the wire.
    pub fill_in: Option<IntentFacts>,
    /// The `FILL_IN_*` bits `Intent.fillIn` reported (0 for an immutable
    /// sender, which never merges).
    pub fill_in_changes: i32,
}

/// One `PendingIntent` the system owns.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingIntentRecord {
    /// The sender type (`getIntentSender`'s first argument).
    pub kind: i32,
    /// The creating package.
    pub package: String,
    /// The caller's request code.
    pub request_code: i32,
    /// The flags the record was created with (including
    /// `FLAG_ONE_SHOT`/`FLAG_IMMUTABLE`; the three ephemeral bits are stripped).
    pub flags: i32,
    /// The intents `getIntentSender` was given.
    pub intents: Vec<IntentFacts>,
    /// The resolved types, index-aligned with `intents`.
    pub resolved_types: Vec<Option<String>>,
    /// The extras argument (`FLAG_UPDATE_CURRENT` replaces them).
    pub extras: Option<Bundle>,
    /// `PendingIntentRecord.canceled`.
    pub cancelled: bool,
    /// Every send the system performed (evidence).
    pub sends: Vec<SendEvidence>,
}

impl PendingIntentRecord {
    /// `PendingIntentRecord.Key` equality, minus the ephemeral flags.
    ///
    /// `intents` compare with `Intent.filterEquals` (extras are not part of the
    /// key, which is why `FLAG_UPDATE_CURRENT` can replace them in place).
    #[must_use]
    pub fn same_key(
        &self,
        kind: i32,
        package: &str,
        request_code: i32,
        flags: i32,
        intents: &[IntentFacts],
    ) -> bool {
        self.kind == kind
            && self.package == package
            && self.request_code == request_code
            && self.flags == flags & !EPHEMERAL_FLAGS
            && self.intents.len() == intents.len()
            && self
                .intents
                .iter()
                .zip(intents)
                .all(|(a, b)| filter_equals(a, b))
    }

    /// The record as the control ops report it.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "package": self.package,
            "request_code": self.request_code,
            "flags": self.flags,
            "cancelled": self.cancelled,
            "intents": self.intents.iter().map(intent_json).collect::<Vec<_>>(),
            "extras": self.extras.as_ref().map(|b| {
                b.entries.iter().map(|(k, v)| serde_json::json!([k, format!("{v:?}")])).collect::<Vec<_>>()
            }),
            "sends": self.sends.iter().map(|s| serde_json::json!({
                "code": s.code,
                "refused_cancelled": s.refused_cancelled,
                "fill_in_ignored": s.fill_in_ignored,
                "fill_in_changes": s.fill_in_changes,
                "fill_in": s.fill_in.as_ref().map(intent_json),
                "outcome": match &s.outcome {
                    Ok(code) => serde_json::json!({"start_result": code}),
                    Err(error) => serde_json::json!({"error": error}),
                },
                "intent": intent_json(&s.intent),
            })).collect::<Vec<_>>(),
        })
    }
}

fn intent_json(intent: &IntentFacts) -> serde_json::Value {
    serde_json::json!({
        "action": intent.action,
        "data": intent.data,
        "package": intent.package,
        "component": intent.component.as_ref().map(|(p, c)| format!("{p}/{c}")),
        "categories": intent.categories,
    })
}

/// `Intent.filterEquals`: action, data, type, identifier, package, component and
/// categories — **not** extras.
fn filter_equals(a: &IntentFacts, b: &IntentFacts) -> bool {
    a.action == b.action
        && a.data == b.data
        && a.package == b.package
        && a.component == b.component
        && a.categories == b.categories
}

/// The sender records plus the pool of `IIntentSender` binders that address
/// them.
///
/// The binders are allocated once, at install time, by
/// [`crate::registry::install`] (`ServiceHost::register_dynamic` needs the
/// object, and the object is a node of the host, not of this table). Slot `i`
/// of `nodes` is the binder that serves `records[i]`.
#[derive(Debug, Default)]
pub struct SenderTable {
    nodes: Vec<u64>,
    records: Vec<Option<PendingIntentRecord>>,
}

impl SenderTable {
    /// Bind the pool of sender binders (install time).
    pub fn bind_nodes(&mut self, nodes: Vec<u64>) {
        self.records = vec![None; nodes.len()];
        self.nodes = nodes;
    }

    /// The number of sender slots.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.nodes.len()
    }

    /// The pool's binder ids, by slot.
    #[must_use]
    pub fn nodes(&self) -> &[u64] {
        &self.nodes
    }

    /// The slot a binder addresses.
    #[must_use]
    pub fn slot_of(&self, binder: u64) -> Option<usize> {
        self.nodes.iter().position(|id| *id == binder)
    }

    /// The record behind a sender binder.
    #[must_use]
    pub fn record(&self, binder: u64) -> Option<&PendingIntentRecord> {
        self.slot_of(binder)
            .and_then(|slot| self.records[slot].as_ref())
    }

    /// Every live record, with the binder that addresses it.
    #[must_use]
    pub fn list(&self) -> Vec<(u64, &PendingIntentRecord)> {
        self.nodes
            .iter()
            .zip(&self.records)
            .filter_map(|(binder, record)| record.as_ref().map(|r| (*binder, r)))
            .collect()
    }

    /// `IActivityManager.getIntentSender`.
    ///
    /// Returns the sender binder, or `None` for `FLAG_NO_CREATE` with no
    /// matching record (the framework's `null`). The caller does the delivery;
    /// see [`fire`].
    pub fn get_intent_sender(
        &mut self,
        kind: i32,
        package: &str,
        request_code: i32,
        flags: i32,
        intents: Vec<IntentFacts>,
        resolved_types: Vec<Option<String>>,
        extras: Option<Bundle>,
    ) -> Result<Option<u64>> {
        if !matches!(
            kind,
            TYPE_ACTIVITY | TYPE_BROADCAST | TYPE_SERVICE | TYPE_FOREGROUND_SERVICE
        ) {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.app.IActivityManager".into(),
                code: 62,
                method: format!("getIntentSender type {kind}"),
            });
        }
        if intents.is_empty() {
            return Err(ServiceError::BadRequest {
                interface: "android.app.IActivityManager".into(),
                method: "getIntentSender".into(),
                detail: "an empty intent array cannot be sent".into(),
            });
        }
        let key_flags = flags & !EPHEMERAL_FLAGS;
        let existing = self.nodes.iter().position(|_| false);
        let _ = existing;
        let found = self.records.iter().position(|record| {
            record
                .as_ref()
                .map(|r| !r.cancelled && r.same_key(kind, package, request_code, flags, &intents))
                .unwrap_or(false)
        });

        if let Some(slot) = found {
            if flags & FLAG_NO_CREATE != 0 {
                return Ok(Some(self.nodes[slot]));
            }
            if flags & FLAG_CANCEL_CURRENT == 0 {
                if flags & FLAG_UPDATE_CURRENT != 0 {
                    if let Some(record) = self.records[slot].as_mut() {
                        record.extras = extras;
                    }
                }
                return Ok(Some(self.nodes[slot]));
            }
            // `FLAG_CANCEL_CURRENT`: the existing record is cancelled and a new
            // one takes its place.
            if let Some(record) = self.records[slot].as_mut() {
                record.cancelled = true;
            }
        } else if flags & FLAG_NO_CREATE != 0 {
            return Ok(None);
        }

        // A fresh record. A free slot is one whose record is absent or already
        // cancelled (AOSP drops a cancelled record from the map).
        let slot = self
            .records
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                self.records
                    .iter()
                    .position(|record| record.as_ref().map(|r| r.cancelled).unwrap_or(true))
            })
            .ok_or_else(|| ServiceError::ResourceLimit {
                what: "pending-intent senders (every IIntentSender node is in use; cancel one first)"
                    .into(),
                limit: self.nodes.len(),
            })?;
        self.records[slot] = Some(PendingIntentRecord {
            kind,
            package: package.to_string(),
            request_code,
            flags: key_flags,
            intents,
            resolved_types,
            extras,
            cancelled: false,
            sends: Vec::new(),
        });
        Ok(Some(self.nodes[slot]))
    }

    /// `IActivityManager.cancelIntentSender`: cancel the record behind `binder`.
    ///
    /// `true` when a live record was cancelled. The binder keeps answering
    /// (it is a pool slot) and refuses further sends, which is what a
    /// cancelled `PendingIntentRecord` does.
    pub fn cancel(&mut self, binder: u64) -> bool {
        match self.slot_of(binder).and_then(|slot| self.records[slot].as_mut()) {
            Some(record) if !record.cancelled => {
                record.cancelled = true;
                true
            }
            _ => false,
        }
    }

    /// Store the evidence of one send against the record behind `binder`.
    pub fn record_send(&mut self, binder: u64, evidence: SendEvidence) {
        if let Some(Some(record)) = self.slot_of(binder).map(|slot| &mut self.records[slot]) {
            if record.flags & FLAG_ONE_SHOT != 0 {
                record.cancelled = true;
            }
            record.sends.push(evidence);
        }
    }

    /// The record behind `binder`, cloned, for a caller that must drop the
    /// borrow before delivering.
    #[must_use]
    pub fn record_cloned(&self, binder: u64) -> Option<PendingIntentRecord> {
        self.record(binder).cloned()
    }
}

/// The bundle `FLAG_UPDATE_CURRENT` stored, as intent extras.
#[must_use]
pub fn bundle_extras(bundle: &Bundle) -> BTreeMap<String, Extra> {
    let mut out = BTreeMap::new();
    for (key, value) in &bundle.entries {
        let extra = match value {
            crate::value::Value::Str(Some(s)) | crate::value::Value::Str8(Some(s)) => {
                Extra::String(s.clone())
            }
            crate::value::Value::Int(i) => Extra::Int(*i),
            crate::value::Value::Bool(b) => Extra::Bool(*b),
            crate::value::Value::StringArray(Some(items)) => Extra::Strings(
                items
                    .iter()
                    .map(|s| s.clone().unwrap_or_default())
                    .collect(),
            ),
            _ => Extra::Present,
        };
        out.insert(key.clone(), extra);
    }
    out
}

/// The binder id of a sender node.
#[must_use]
pub fn binder_id(id: u64) -> BinderId {
    BinderId(id)
}

/// One `android.content.IIntentSender` node.
///
/// The descriptor has no entry in the decoded `acos-aidl` tables
/// (`IIntentSender.aidl` is a hand-written AIDL interface with a single oneway
/// method), so the node serves the transaction itself: code
/// [`TRANSACTION_SEND`] replays the record behind *this* slot. The reply body is
/// empty (oneway); a refusal is evidence on the record, exactly as
/// `PendingIntentRecord.sendInner` logs and returns.
pub struct IntentSenderService {
    atms: std::sync::Arc<std::sync::Mutex<crate::atms::AtmsState>>,
    slot: usize,
}

/// The AIDL descriptor of the sender node.
pub const DESCRIPTOR: &str = "android.content.IIntentSender";

/// How many `IIntentSender` nodes a session pre-allocates.
pub const SENDER_NODES: usize = 16;

impl IntentSenderService {
    /// A node for `slot` over the session's ATMS state.
    #[must_use]
    pub fn new(
        atms: std::sync::Arc<std::sync::Mutex<crate::atms::AtmsState>>,
        slot: usize,
    ) -> Self {
        Self { atms, slot }
    }
}

impl crate::host::ServiceObject for IntentSenderService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: format!("{interface} (IIntentSender is a raw node)"),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        _reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        if code != TRANSACTION_SEND {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: DESCRIPTOR.to_string(),
                code,
                method: format!("IIntentSender code {code} (only send is modelled)"),
            });
        }
        // `send(int code, in Intent intent, String resolvedType, in IBinder
        // whitelistToken, IIntentReceiver finishedReceiver, String
        // requiredPermission, in Bundle options)`. The `code` argument is what
        // the evidence reports.
        let code_arg = data.read_int32().unwrap_or(0);
        // The fill-in is a nullable parcelable (`writeTypedObject`): a `1`
        // marker means an `Intent` body follows. An **immutable** sender never
        // merges it (`PendingIntentRecord.sendInner` skips `fillIn` for
        // `FLAG_IMMUTABLE`), so its body is not decoded at all — a body this
        // codec cannot read (a non-null `Uri`, a `ClipData`) must not turn a
        // send the framework would perform into an error. A **mutable** sender
        // does merge it, so its body must decode: an unreadable fill-in is the
        // structured `UnsupportedParcelable`, never a send without it.
        let has_fill_in = data.read_parcelable_marker().unwrap_or(false);
        let mutable = {
            let state = self
                .atms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let binder = state
                .ams
                .intent_senders
                .nodes()
                .get(self.slot)
                .copied()
                .unwrap_or(0);
            state
                .ams
                .intent_senders
                .record(binder)
                .map(|record| record.flags & FLAG_IMMUTABLE == 0)
                .unwrap_or(false)
        };
        let fill_in_facts = if has_fill_in && mutable {
            let value = crate::parcelable::read_intent(data)?;
            Some(IntentFacts::from_parcelable(&value))
        } else {
            None
        };
        let fill_in = match &fill_in_facts {
            Some(facts) => FillIn::Intent(facts),
            None if has_fill_in => FillIn::Ignored,
            None => FillIn::None,
        };
        let mut state = self
            .atms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let binder = state
            .ams
            .intent_senders
            .nodes()
            .get(self.slot)
            .copied()
            .unwrap_or(0);
        let evidence = state
            .ams
            .send_pending_intent_with_fill_in(binder, code_arg, fill_in);
        let _ = evidence;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::ServiceObject as _;

    fn activity(package: &str, class: &str) -> IntentFacts {
        IntentFacts::explicit(package, class)
    }

    fn table() -> SenderTable {
        let mut table = SenderTable::default();
        table.bind_nodes(vec![100, 101, 102, 103]);
        table
    }

    #[test]
    fn the_same_key_returns_the_same_sender_and_no_create_never_creates() {
        let mut table = table();
        let intent = activity("dev.acos.fixtures.notif", "MainActivity");
        let first = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                7,
                FLAG_IMMUTABLE,
                vec![intent.clone()],
                vec![None],
                None,
            )
            .expect("allocate")
            .expect("a binder");
        let again = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                7,
                FLAG_IMMUTABLE,
                vec![intent.clone()],
                vec![None],
                None,
            )
            .expect("reuse")
            .expect("a binder");
        assert_eq!(first, again, "an equal key is the same record");
        assert_eq!(table.list().len(), 1);

        // NO_CREATE for a key that does not exist answers null.
        let missing = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                8,
                FLAG_NO_CREATE,
                vec![intent.clone()],
                vec![None],
                None,
            )
            .expect("no-create");
        assert!(missing.is_none(), "FLAG_NO_CREATE never creates a record");
        assert_eq!(table.list().len(), 1);
        // NO_CREATE for a key that does exist answers its binder. The key keeps
        // every flag but the three ephemeral ones, so the lookup has to pass the
        // same `FLAG_IMMUTABLE` the record was created with (AOSP: `flags &=
        // ~(NO_CREATE|CANCEL_CURRENT|UPDATE_CURRENT)`).
        let existing = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                7,
                FLAG_IMMUTABLE | FLAG_NO_CREATE,
                vec![intent],
                vec![None],
                None,
            )
            .expect("no-create")
            .expect("the existing record");
        assert_eq!(existing, first);
    }

    #[test]
    fn update_current_replaces_the_extras_and_cancel_current_replaces_the_record() {
        let mut table = table();
        let intent = activity("dev.acos.fixtures.notif", "MainActivity");
        let mut extras = Bundle::new();
        extras.put("android.title", crate::value::Value::Str(Some("first".into())));
        let binder = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                3,
                FLAG_UPDATE_CURRENT,
                vec![intent.clone()],
                vec![None],
                Some(extras.clone()),
            )
            .expect("allocate")
            .expect("a binder");
        // A second UPDATE_CURRENT with different extras keeps the record and the
        // binder, and replaces the stored extras (`PendingIntentRecord.
        // replaceExtras`).
        let mut other = Bundle::new();
        other.put("android.title", crate::value::Value::Str(Some("second".into())));
        let same = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                3,
                FLAG_UPDATE_CURRENT,
                vec![intent.clone()],
                vec![None],
                Some(other.clone()),
            )
            .expect("update")
            .expect("a binder");
        assert_eq!(same, binder);
        assert_eq!(
            table.record(binder).and_then(|r| r.extras.clone()),
            Some(other),
            "FLAG_UPDATE_CURRENT replaces the extras"
        );

        // CANCEL_CURRENT cancels the record and allocates a new slot.
        let cancelled = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                3,
                FLAG_CANCEL_CURRENT,
                vec![intent.clone()],
                vec![None],
                None,
            )
            .expect("cancel current")
            .expect("a binder");
        assert_ne!(cancelled, binder, "CANCEL_CURRENT allocates a new record");
        assert!(
            table.record(binder).map(|r| r.cancelled).unwrap_or(false)
                || table.record(binder).is_none(),
            "the previous record is cancelled"
        );
    }

    #[test]
    fn cancel_and_one_shot_are_evidence_not_silence() {
        let mut table = table();
        let intent = activity("dev.acos.fixtures.notif", "MainActivity");
        let binder = table
            .get_intent_sender(
                TYPE_BROADCAST,
                "dev.acos.fixtures.notif",
                1,
                FLAG_ONE_SHOT,
                vec![intent.clone()],
                vec![None],
                None,
            )
            .expect("allocate")
            .expect("a binder");
        table.record_send(
            binder,
            SendEvidence {
                code: 0,
                outcome: Ok(0),
                intent,
                refused_cancelled: false,
                fill_in_ignored: false,
                fill_in: None,
                fill_in_changes: 0,
            },
        );
        let record = table.record(binder).expect("record");
        assert!(record.cancelled, "FLAG_ONE_SHOT cancels after one send");
        assert_eq!(record.sends.len(), 1);
        assert!(!table.cancel(binder), "an already-cancelled record is not cancelled twice");
    }

    #[test]
    fn the_pool_refuses_to_hand_out_more_senders_than_it_has_nodes() {
        let mut table = table();
        for request in 0..4 {
            table
                .get_intent_sender(
                    TYPE_SERVICE,
                    "dev.acos.fixtures.notif",
                    request,
                    0,
                    vec![IntentFacts::explicit("pkg", format!("Service{request}"))],
                    vec![None],
                    None,
                )
                .expect("allocate")
                .expect("a binder");
        }
        let error = table
            .get_intent_sender(
                TYPE_SERVICE,
                "dev.acos.fixtures.notif",
                9,
                0,
                vec![IntentFacts::explicit("pkg", "Service9")],
                vec![None],
                None,
            )
            .expect_err("the pool is full");
        assert_eq!(error.code(), crate::codes::ART_RESOURCE_LIMIT);
    }

    /// `send`'s request body: `code`, the nullable fill-in marker and the real
    /// `Intent.writeToParcel` body.
    fn send_request(code: i32, fill_in: Option<&IntentFacts>) -> acos_parcel::Parcel {
        let mut parcel = acos_parcel::Parcel::new();
        parcel.write_int32(code);
        parcel.write_parcelable_marker(fill_in.is_some());
        if let Some(fill_in) = fill_in {
            crate::parcelable::write_intent(&mut parcel, &fill_in.to_parcelable())
                .expect("encode the fill-in");
        }
        // A request parcel arrives at position 0; `write_*` leaves the cursor at
        // the end.
        parcel.set_data_position(0);
        parcel
    }

    /// A request whose fill-in body is one this codec refuses (a non-null
    /// `android.net.Uri`), to prove the sender decision comes *before* the
    /// decode.
    fn uris_fill_in_request(code: i32) -> acos_parcel::Parcel {
        let mut parcel = acos_parcel::Parcel::new();
        parcel.write_int32(code);
        parcel.write_parcelable_marker(true);
        parcel.write_string8_opt(None).expect("action");
        parcel.write_int32(1); // data presence word: a `Uri` body follows
        parcel.set_data_position(0);
        parcel
    }

    fn sender_node(flags: i32) -> (IntentSenderService, u64) {
        let mut state = crate::atms::AtmsState::new();
        let mut table = SenderTable::default();
        table.bind_nodes(vec![500]);
        let binder = table
            .get_intent_sender(
                TYPE_ACTIVITY,
                "dev.acos.fixtures.notif",
                0,
                flags,
                vec![activity("dev.acos.fixtures.notif", "Target")],
                vec![None],
                None,
            )
            .expect("getIntentSender")
            .expect("a binder");
        state.ams.intent_senders = table;
        let state = std::sync::Arc::new(std::sync::Mutex::new(state));
        (IntentSenderService::new(state, 0), binder)
    }

    /// PIFILLIN: `PendingIntentRecord.sendInner` merges a caller's fill-in into
    /// a **mutable** sender and delivers the merged intent. The extras merge is
    /// the observable: the fill-in's key lands in the delivered intent and the
    /// evidence reports both the fill-in and the `FILL_IN_*` change mask.
    #[test]
    fn a_fill_in_on_a_mutable_sender_is_merged_and_delivered() {
        let (mut node, binder) = sender_node(FLAG_MUTABLE);
        let fill_in = IntentFacts::action("fill.action")
            .with_string_extra("location", "37.4220,-122.0841");
        let mut data = send_request(0, Some(&fill_in));
        let mut reply = acos_parcel::Parcel::new();
        node.raw_transact(DESCRIPTOR, TRANSACTION_SEND, &mut data, &mut reply)
            .expect("a mutable sender merges the fill-in");
        let state = node.atms.lock().expect("state");
        let record = state.ams.intent_senders.record(binder).expect("record");
        assert_eq!(record.sends.len(), 1, "the send is recorded once");
        let evidence = &record.sends[0];
        assert!(!evidence.fill_in_ignored, "a mutable sender never ignores it");
        assert_eq!(evidence.fill_in_changes, crate::fill_in::FILL_IN_ACTION);
        assert_eq!(
            evidence.intent.action.as_deref(),
            Some("fill.action"),
            "the delivered intent is the merged one"
        );
        assert!(
            matches!(
                evidence.intent.extras.get("location"),
                Some(crate::ams::Extra::String(value)) if value == "37.4220,-122.0841"
            ),
            "the fill-in's extra reached the delivered intent: {:?}",
            evidence.intent.extras
        );
        // The stored record is not mutated (AOSP merges into a copy).
        assert_eq!(
            record.intents[0].action, None,
            "the record still replays its stored intent"
        );
    }

    /// The immutable case the notification tap uses: `sendInner` skips
    /// `Intent.fillIn` for `FLAG_IMMUTABLE`, so the stored intent is delivered
    /// unchanged and the evidence names the ignored fill-in.
    #[test]
    fn a_fill_in_on_an_immutable_sender_is_ignored_and_named() {
        let (mut node, binder) = sender_node(FLAG_IMMUTABLE);
        let fill_in = IntentFacts::action("fill.action").with_string_extra("k", "v");
        let mut data = send_request(0, Some(&fill_in));
        let mut reply = acos_parcel::Parcel::new();
        node.raw_transact(DESCRIPTOR, TRANSACTION_SEND, &mut data, &mut reply)
            .expect("an immutable sender still sends");
        let state = node.atms.lock().expect("state");
        let record = state.ams.intent_senders.record(binder).expect("record");
        assert_eq!(record.sends.len(), 1);
        let evidence = &record.sends[0];
        assert!(evidence.fill_in_ignored, "the fill-in is named on the evidence");
        assert_eq!(evidence.fill_in_changes, 0, "nothing was merged");
        assert_eq!(evidence.code, 0);
        assert_eq!(evidence.intent.action, None, "the stored intent was delivered");
        assert!(evidence.intent.extras.is_empty());
    }

    /// The sender decision comes **before** the decode: an immutable sender's
    /// fill-in is ignored, so a body this codec cannot read must not fail a
    /// send the framework performs. A mutable sender must read it, and an
    /// unreadable fill-in there is the structured refusal.
    #[test]
    fn an_unreadable_fill_in_is_ignored_by_an_immutable_sender_and_refused_by_a_mutable_one() {
        let (mut immutable, _) = sender_node(FLAG_IMMUTABLE);
        let mut data = uris_fill_in_request(0);
        let mut reply = acos_parcel::Parcel::new();
        immutable
            .raw_transact(DESCRIPTOR, TRANSACTION_SEND, &mut data, &mut reply)
            .expect("an immutable sender never reads the fill-in it ignores");

        let (mut mutable, binder) = sender_node(FLAG_MUTABLE);
        let mut data = uris_fill_in_request(0);
        let mut reply = acos_parcel::Parcel::new();
        let error = mutable
            .raw_transact(DESCRIPTOR, TRANSACTION_SEND, &mut data, &mut reply)
            .expect_err("a mutable sender cannot merge a body it cannot read");
        assert_eq!(
            error.code(),
            crate::codes::SERVICE_UNSUPPORTED_PARCELABLE,
            "the refusal is the unreadable parcelable, not a silent send: {error:?}"
        );
        let state = mutable.atms.lock().expect("state");
        let record = state.ams.intent_senders.record(binder).expect("record");
        assert!(
            record.sends.is_empty(),
            "a refused send is not evidence of a delivery: {:?}",
            record.sends
        );
    }
}
