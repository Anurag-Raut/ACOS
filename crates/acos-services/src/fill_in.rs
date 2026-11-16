//! `Intent.fillIn(Intent other, int flags)` — the AOSP merge (PIFILLIN).
//!
//! `PendingIntent.send(context, code, intent)` hands the system a **fill-in**
//! intent: `PendingIntentRecord.sendInner` copies the stored intent, merges the
//! caller's intent into the copy and delivers the copy. The merge is
//! `Intent.fillIn` (`base/core/java/android/content/Intent.java:11530-11615`),
//! and this module is the one implementation of it:
//!
//! * [`fill_in_value`] works on the real `android.content.Intent` value the
//!   codec reads and writes ([`crate::parcelable::read_intent`] /
//!   [`crate::parcelable::write_intent`]); the Java differential
//!   (`crates/acos-runtime/tests/parcelable_java.rs::fill_in_java`) pins it
//!   byte-for-byte against a real `Intent.fillIn` + `writeToParcel`.
//! * [`crate::ams::IntentFacts::fill_in`] runs the same function over the
//!   reduced facts the AMS state machine keeps, so the reduced path cannot
//!   drift from the byte-verified one.
//!
//! ## The rules, exactly as AOSP writes them
//!
//! Every field is copied **only when the target does not define it**, unless
//! the matching `FILL_IN_*` flag allows overwriting:
//!
//! | field | condition |
//! |---|---|
//! | action | `other.action != null && (self.action == null \|\| FILL_IN_ACTION)` |
//! | data **and** type (one field) | `(other.data != null \|\| other.type != null) && ((self.data == null && self.type == null) \|\| FILL_IN_DATA)`; both slots are assigned (so `other.type`-only clears `self.data`) |
//! | identifier | `other.identifier != null && (self.identifier == null \|\| FILL_IN_IDENTIFIER)` |
//! | categories | `other.categories != null && (self.categories == null \|\| FILL_IN_CATEGORIES)` |
//! | package | `other.package != null && (self.package == null \|\| FILL_IN_PACKAGE) && self.selector == null` |
//! | selector | only with `FILL_IN_SELECTOR`, and only when `self.package == null` |
//! | clipData | `other.clipData != null && (self.clipData == null \|\| FILL_IN_CLIP_DATA)` |
//! | component | only with `FILL_IN_COMPONENT` (a sender must not redirect an intent) |
//! | flags | **always** `self.flags \|= other.flags` |
//! | sourceBounds | `other != null && (self == null \|\| FILL_IN_SOURCE_BOUNDS)` |
//! | extras | key by key: the fill-in's keys are added, the stored intent's keys win (`newb = new Bundle(other.mExtras); newb.putAll(mExtras)`) |
//! | contentUserHint | when a Uri may have been copied: `self == USER_CURRENT && other != USER_CURRENT` |
//!
//! The selector rule assigns `mPackage = null` after copying the selector; the
//! guard is `self.package == null`, so that assignment cannot change anything —
//! it is kept as the rule is written. `mExtendedFlags` (`|= other.mExtendedFlags`)
//! is not a field this image's `Intent.writeToParcel` writes and the codec does
//! not carry it; the merge of `mFlags` is modelled, the extended word is not.
//!
//! ## Immutable senders
//!
//! The merge above is the **mutable** path. `PendingIntentRecord.sendInner`
//! skips it entirely when the key carries `FLAG_IMMUTABLE` (`services.dex`,
//! `Lcom/android/server/am/PendingIntentRecord;->sendInner`, `0x04000000` test
//! before the `fillIn` call), so an immutable sender delivers its stored intent
//! and only the send-time flags-mask arguments still apply. That decision is
//! made by [`crate::pending_intent`], not here.

use crate::ams::IntentFacts;
use crate::value::{Bundle, Parcelable, Value};

/// `Intent.FILL_IN_ACTION`.
pub const FILL_IN_ACTION: i32 = 1 << 0;
/// `Intent.FILL_IN_DATA` (data **and** type).
pub const FILL_IN_DATA: i32 = 1 << 1;
/// `Intent.FILL_IN_CATEGORIES`.
pub const FILL_IN_CATEGORIES: i32 = 1 << 2;
/// `Intent.FILL_IN_COMPONENT`.
pub const FILL_IN_COMPONENT: i32 = 1 << 3;
/// `Intent.FILL_IN_PACKAGE`.
pub const FILL_IN_PACKAGE: i32 = 1 << 4;
/// `Intent.FILL_IN_SOURCE_BOUNDS`.
pub const FILL_IN_SOURCE_BOUNDS: i32 = 1 << 5;
/// `Intent.FILL_IN_SELECTOR`.
pub const FILL_IN_SELECTOR: i32 = 1 << 6;
/// `Intent.FILL_IN_CLIP_DATA`.
pub const FILL_IN_CLIP_DATA: i32 = 1 << 7;
/// `Intent.FILL_IN_IDENTIFIER`.
pub const FILL_IN_IDENTIFIER: i32 = 1 << 8;

/// `UserHandle.USER_CURRENT`, the "no hint" value of `Intent.mContentUserHint`
/// and of `PendingIntent` send options.
pub const USER_CURRENT: i32 = -2;

/// `Intent.fillIn(other, flags)` over the real Intent value.
///
/// `target` is mutated in place; both arguments must be
/// [`crate::parcelable::INTENT_CLASS`] values with
/// [`crate::parcelable::INTENT_FIELDS`] fields. Returns the `FILL_IN_*` bits of
/// the fields that changed (AOSP's return value).
pub fn fill_in_value(target: &mut Parcelable, other: &Parcelable, flags: i32) -> crate::Result<i32> {
    let Parcelable::Ordered {
        class: target_class,
        values: target_values,
    } = target
    else {
        return Err(bad("target is not an Intent"));
    };
    if target_class != crate::parcelable::INTENT_CLASS
        || target_values.len() != crate::parcelable::INTENT_FIELDS
    {
        return Err(bad("target is not an Intent"));
    }
    let Parcelable::Ordered {
        class: other_class,
        values: other_values,
    } = other
    else {
        return Err(bad("the fill-in is not an Intent"));
    };
    if other_class != crate::parcelable::INTENT_CLASS
        || other_values.len() != crate::parcelable::INTENT_FIELDS
    {
        return Err(bad("the fill-in is not an Intent"));
    }

    use crate::parcelable::{
        INTENT_ACTION, INTENT_CATEGORIES, INTENT_CLIP_DATA, INTENT_COMPONENT,
        INTENT_CONTENT_USER_HINT, INTENT_DATA, INTENT_EXTRAS, INTENT_FLAGS, INTENT_IDENTIFIER,
        INTENT_PACKAGE, INTENT_SELECTOR, INTENT_SOURCE_BOUNDS, INTENT_TYPE,
    };

    let mut changes = 0;
    let mut may_have_copied_uris = false;

    // action
    if let Some(action) = str_at(other_values, INTENT_ACTION) {
        if str_at(target_values, INTENT_ACTION).is_none() || flags & FILL_IN_ACTION != 0 {
            target_values[INTENT_ACTION] = Value::Str(Some(action.to_string()));
            changes |= FILL_IN_ACTION;
        }
    }

    // data + type are one field: `other` having *either* makes the pair
    // fillable, and both slots are assigned (AOSP assigns `mData` and `mType`).
    let other_data = str_at(other_values, INTENT_DATA);
    let other_type = str_at(other_values, INTENT_TYPE);
    if other_data.is_some() || other_type.is_some() {
        let empty = str_at(target_values, INTENT_DATA).is_none()
            && str_at(target_values, INTENT_TYPE).is_none();
        if empty || flags & FILL_IN_DATA != 0 {
            target_values[INTENT_DATA] =
                Value::Str(other_data.map(str::to_string));
            target_values[INTENT_TYPE] =
                Value::Str(other_type.map(str::to_string));
            changes |= FILL_IN_DATA;
            may_have_copied_uris = true;
        }
    }

    // identifier
    if let Some(identifier) = str_at(other_values, INTENT_IDENTIFIER) {
        if str_at(target_values, INTENT_IDENTIFIER).is_none() || flags & FILL_IN_IDENTIFIER != 0 {
            target_values[INTENT_IDENTIFIER] = Value::Str(Some(identifier.to_string()));
            changes |= FILL_IN_IDENTIFIER;
        }
    }

    // categories: "defined" is a non-null ArraySet. `read_intent` answers null
    // for a count of 0, which is exactly `Intent.readFromParcel`.
    if is_set(&other_values[INTENT_CATEGORIES])
        && (!is_set(&target_values[INTENT_CATEGORIES]) || flags & FILL_IN_CATEGORIES != 0)
    {
        target_values[INTENT_CATEGORIES] = other_values[INTENT_CATEGORIES].clone();
        changes |= FILL_IN_CATEGORIES;
    }

    // package (only while the target has no selector)
    if let Some(package) = str_at(other_values, INTENT_PACKAGE) {
        let target_selector_null = is_null_parcelable(&target_values[INTENT_SELECTOR]);
        if (str_at(target_values, INTENT_PACKAGE).is_none() || flags & FILL_IN_PACKAGE != 0)
            && target_selector_null
        {
            target_values[INTENT_PACKAGE] = Value::Str(Some(package.to_string()));
            changes |= FILL_IN_PACKAGE;
        }
    }

    // selector: explicit permission only, and only while the target has no package
    if !is_null_parcelable(&other_values[INTENT_SELECTOR]) && flags & FILL_IN_SELECTOR != 0 {
        if str_at(target_values, INTENT_PACKAGE).is_none() {
            target_values[INTENT_SELECTOR] = other_values[INTENT_SELECTOR].clone();
            // AOSP: `mPackage = null;` — a no-op behind the guard above.
            target_values[INTENT_PACKAGE] = Value::Str(None);
            changes |= FILL_IN_SELECTOR;
        }
    }

    // clipData
    if !is_null_parcelable(&other_values[INTENT_CLIP_DATA])
        && (is_null_parcelable(&target_values[INTENT_CLIP_DATA])
            || flags & FILL_IN_CLIP_DATA != 0)
    {
        target_values[INTENT_CLIP_DATA] = other_values[INTENT_CLIP_DATA].clone();
        changes |= FILL_IN_CLIP_DATA;
        may_have_copied_uris = true;
    }

    // component: explicit permission only
    if !is_null_parcelable(&other_values[INTENT_COMPONENT]) && flags & FILL_IN_COMPONENT != 0 {
        target_values[INTENT_COMPONENT] = other_values[INTENT_COMPONENT].clone();
        changes |= FILL_IN_COMPONENT;
    }

    // flags are merged unconditionally (AOSP `mFlags |= other.mFlags`).
    let target_flags = target_values[INTENT_FLAGS].as_int().unwrap_or(0);
    let other_flags = other_values[INTENT_FLAGS].as_int().unwrap_or(0);
    target_values[INTENT_FLAGS] = Value::Int(target_flags | other_flags);

    // sourceBounds
    if !is_null_parcelable(&other_values[INTENT_SOURCE_BOUNDS])
        && (is_null_parcelable(&target_values[INTENT_SOURCE_BOUNDS])
            || flags & FILL_IN_SOURCE_BOUNDS != 0)
    {
        target_values[INTENT_SOURCE_BOUNDS] = other_values[INTENT_SOURCE_BOUNDS].clone();
        changes |= FILL_IN_SOURCE_BOUNDS;
    }

    // extras: `new Bundle(other.mExtras); newb.putAll(mExtras)` — the fill-in's
    // keys are added and the stored intent's keys win.
    let target_extras = target_values[INTENT_EXTRAS].as_bundle().cloned();
    let other_extras = other_values[INTENT_EXTRAS].as_bundle().cloned();
    match (target_extras, other_extras) {
        (None, Some(fill_in)) => {
            target_values[INTENT_EXTRAS] = Value::Bundle(Some(fill_in));
            may_have_copied_uris = true;
        }
        (Some(stored), Some(fill_in)) => {
            let mut merged = fill_in.clone();
            for (key, value) in stored.entries {
                merged.put(&key, value);
            }
            target_values[INTENT_EXTRAS] = Value::Bundle(Some(merged));
            may_have_copied_uris = true;
        }
        (None, None) => {}
        (Some(_), None) => {}
    }

    // contentUserHint: only after a Uri may have been copied.
    let target_hint = target_values[INTENT_CONTENT_USER_HINT]
        .as_int()
        .unwrap_or(USER_CURRENT);
    let other_hint = other_values[INTENT_CONTENT_USER_HINT]
        .as_int()
        .unwrap_or(USER_CURRENT);
    if may_have_copied_uris && target_hint == USER_CURRENT && other_hint != USER_CURRENT {
        target_values[INTENT_CONTENT_USER_HINT] = Value::Int(other_hint);
    }

    Ok(changes)
}

/// Replace the extras of an Intent value with `extras` (the `FLAG_UPDATE_CURRENT`
/// extras), keeping a null bundle null.
pub fn set_value_extras(target: &mut Parcelable, extras: &Bundle) {
    if let Parcelable::Ordered { values, .. } = target {
        if values.len() == crate::parcelable::INTENT_FIELDS {
            values[crate::parcelable::INTENT_EXTRAS] = Value::Bundle(Some(extras.clone()));
        }
    }
}

fn str_at<'a>(values: &'a [Value], index: usize) -> Option<&'a str> {
    values.get(index).and_then(Value::as_str)
}

fn is_set(value: &Value) -> bool {
    matches!(
        value,
        Value::StringList(Some(_)) | Value::StringArray(Some(_))
    )
}

fn is_null_parcelable(value: &Value) -> bool {
    match value {
        Value::Parcelable(Parcelable::Null) => true,
        Value::Parcelable(_) => false,
        // A field the codec fills with a non-parcelable value is not a body.
        _ => true,
    }
}

fn bad(detail: &str) -> crate::ServiceError {
    crate::ServiceError::BadRequest {
        interface: crate::parcelable::INTENT_CLASS.into(),
        method: "fillIn".into(),
        detail: detail.into(),
    }
}

/// Convenience wrapper: `target.fill_in(other, flags)` over [`IntentFacts`].
pub fn fill_in_facts(target: &mut IntentFacts, other: &IntentFacts, flags: i32) -> i32 {
    target.fill_in(other, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> Parcelable {
        IntentFacts::default().to_parcelable()
    }

    fn with(mutate: impl FnOnce(&mut IntentFacts)) -> Parcelable {
        let mut facts = IntentFacts::default();
        mutate(&mut facts);
        facts.to_parcelable()
    }

    fn facts(value: &Parcelable) -> IntentFacts {
        IntentFacts::from_parcelable(value)
    }

    #[test]
    fn an_empty_target_takes_every_fillable_field_but_never_the_component() {
        let mut target = intent();
        let other = with(|o| {
            o.action = Some("android.intent.action.VIEW".into());
            o.data = Some("content://example/1".into());
            o.type_ = Some("text/plain".into());
            o.identifier = Some("id-1".into());
            o.package = Some("dev.acos.fixtures".into());
            o.component = Some(("dev.acos.fixtures".into(), "Target".into()));
            o.categories = vec!["android.intent.category.DEFAULT".into()];
            o.source_bounds = Some([1, 2, 3, 4]);
            o.flags = 0x1000_0000;
            o.extras.insert("kept".into(), crate::ams::Extra::String("yes".into()));
        });
        let changes = fill_in_value(&mut target, &other, 0).expect("fill in");
        let merged = facts(&target);
        assert_eq!(
            changes,
            FILL_IN_ACTION
                | FILL_IN_DATA
                | FILL_IN_IDENTIFIER
                | FILL_IN_CATEGORIES
                | FILL_IN_PACKAGE
                | FILL_IN_SOURCE_BOUNDS
        );
        assert_eq!(merged.action.as_deref(), Some("android.intent.action.VIEW"));
        assert_eq!(merged.data.as_deref(), Some("content://example/1"));
        assert_eq!(merged.type_.as_deref(), Some("text/plain"));
        assert_eq!(merged.identifier.as_deref(), Some("id-1"));
        assert_eq!(merged.package.as_deref(), Some("dev.acos.fixtures"));
        assert_eq!(merged.source_bounds, Some([1, 2, 3, 4]));
        assert_eq!(merged.flags, 0x1000_0000, "flags merge unconditionally");
        assert_eq!(merged.extras.len(), 1);
        assert_eq!(
            merged.component, None,
            "the component is only copied with FILL_IN_COMPONENT"
        );
        assert_eq!(merged.selector, None);
    }

    #[test]
    fn the_fill_in_flags_allow_overwriting_a_defined_field() {
        let mut target = with(|t| {
            t.action = Some("stored".into());
            t.categories = vec!["stored".into()];
            t.package = Some("stored.pkg".into());
            t.component = Some(("stored.pkg".into(), "Stored".into()));
        });
        let other = with(|o| {
            o.action = Some("fill".into());
            o.categories = vec!["fill".into()];
            o.package = Some("fill.pkg".into());
            o.component = Some(("fill.pkg".into(), "Fill".into()));
        });
        // Without the flags nothing defined is replaced.
        let mut unchanged = target.clone();
        assert_eq!(fill_in_value(&mut unchanged, &other, 0).expect("merge"), 0);
        assert_eq!(facts(&unchanged), facts(&target));

        let changes = fill_in_value(
            &mut target,
            &other,
            FILL_IN_ACTION | FILL_IN_CATEGORIES | FILL_IN_PACKAGE | FILL_IN_COMPONENT,
        )
        .expect("merge");
        assert_eq!(
            changes,
            FILL_IN_ACTION | FILL_IN_CATEGORIES | FILL_IN_PACKAGE | FILL_IN_COMPONENT
        );
        let merged = facts(&target);
        assert_eq!(merged.action.as_deref(), Some("fill"));
        assert_eq!(merged.categories, vec!["fill".to_string()]);
        assert_eq!(merged.package.as_deref(), Some("fill.pkg"));
        assert_eq!(
            merged.component,
            Some(("fill.pkg".into(), "Fill".into()))
        );
    }

    #[test]
    fn data_and_type_are_one_field_and_the_fill_in_type_clears_the_stored_data() {
        let mut target = with(|t| t.data = Some("content://stored".into()));
        let other = with(|o| o.type_ = Some("image/png".into()));
        let changes = fill_in_value(&mut target, &other, 0).expect("merge");
        // The target has data, so `(mData == null && mType == null)` is false and
        // no flag is set: nothing is copied.
        assert_eq!(changes, 0);
        assert_eq!(facts(&target).data.as_deref(), Some("content://stored"));

        let changes = fill_in_value(&mut target, &other, FILL_IN_DATA).expect("merge");
        assert_eq!(changes, FILL_IN_DATA);
        let merged = facts(&target);
        assert_eq!(merged.type_.as_deref(), Some("image/png"));
        assert_eq!(
            merged.data, None,
            "AOSP assigns the whole data/type pair, so a type-only fill-in clears mData"
        );
    }

    #[test]
    fn the_package_is_not_filled_in_while_a_selector_is_set_and_the_selector_needs_its_flag() {
        let selector = with(|s| s.action = Some("inner".into()));
        let mut target = with(|t| {
            t.selector = Some(Box::new(facts(&selector)));
        });
        let other = with(|o| {
            o.package = Some("fill.pkg".into());
            o.component = Some(("fill.pkg".into(), "Target".into()));
        });
        // A selector-only fill-in (no package: the package rule runs first and
        // would define the target's package, which then blocks the selector —
        // AOSP's own order).
        let mut other_with_selector = with(|o| o.action = Some("fill".into()));
        let Parcelable::Ordered { values, .. } = &mut other_with_selector else {
            unreachable!("an Intent value")
        };
        values[crate::parcelable::INTENT_SELECTOR] = Value::Parcelable(selector.clone());
        // The selector is never copied without FILL_IN_SELECTOR, and while it is
        // set the package is not filled in.
        let mut unchanged = target.clone();
        assert_eq!(fill_in_value(&mut unchanged, &other, 0).expect("merge"), 0);
        assert_eq!(facts(&unchanged), facts(&target));

        // With FILL_IN_SELECTOR on a target that has no package, the selector is
        // copied.
        let mut with_selector = with(|t| t.action = Some("outer".into()));
        let changes = fill_in_value(&mut with_selector, &other_with_selector, FILL_IN_SELECTOR)
            .expect("merge");
        assert_eq!(changes, FILL_IN_SELECTOR);
        assert_eq!(
            facts(&with_selector)
                .selector
                .as_deref()
                .and_then(|s| s.action.as_deref()),
            Some("inner")
        );

        // A target with a selector does not take the fill-in package either.
        let changes = fill_in_value(&mut target, &other, FILL_IN_PACKAGE).expect("merge");
        assert_eq!(changes, 0);
        assert_eq!(facts(&target).package, None);

        // AOSP's field order: the package rule runs before the selector rule, so
        // a fill-in carrying both defines the target's package and the selector
        // is then refused (it requires `mPackage == null`).
        let mut both_other = other_with_selector.clone();
        let Parcelable::Ordered { values, .. } = &mut both_other else {
            unreachable!("an Intent value")
        };
        values[crate::parcelable::INTENT_PACKAGE] = Value::Str(Some("fill.pkg".into()));
        let mut fresh = with(|t| t.action = Some("outer".into()));
        let changes = fill_in_value(&mut fresh, &both_other, FILL_IN_SELECTOR).expect("merge");
        assert_eq!(changes, FILL_IN_PACKAGE, "the package wins, the selector is blocked");
        assert_eq!(facts(&fresh).package.as_deref(), Some("fill.pkg"));
        assert_eq!(facts(&fresh).selector, None);
    }

    #[test]
    fn extras_merge_key_by_key_and_the_stored_value_wins() {
        let mut target = with(|t| {
            t.extras
                .insert("stored".into(), crate::ams::Extra::String("mine".into()));
            t.extras
                .insert("shared".into(), crate::ams::Extra::Int(1));
        });
        let other = with(|o| {
            o.extras
                .insert("fresh".into(), crate::ams::Extra::Bool(true));
            o.extras
                .insert("shared".into(), crate::ams::Extra::Int(2));
        });
        fill_in_value(&mut target, &other, 0).expect("merge");
        let merged = facts(&target);
        assert_eq!(
            merged.extras.get("fresh"),
            Some(&crate::ams::Extra::Bool(true)),
            "the fill-in's keys are added"
        );
        assert_eq!(
            merged.extras.get("shared"),
            Some(&crate::ams::Extra::Int(1)),
            "the stored intent's value wins (newb.putAll(mExtras))"
        );
        assert_eq!(merged.extras.len(), 3);
        // The bundled bytes really carry the merged map.
        let reparsed = facts(&target);
        assert_eq!(reparsed.extras, merged.extras);
    }

    #[test]
    fn a_null_stored_extras_bundle_takes_the_fill_in_bundle_whole() {
        let mut target = intent();
        let other = with(|o| {
            o.extras
                .insert("location".into(), crate::ams::Extra::Int(7));
        });
        assert_eq!(
            fill_in_value(&mut target, &other, 0).expect("merge"),
            0,
            "extras are not part of the FILL_IN_* change mask"
        );
        assert_eq!(
            facts(&target).extras.get("location"),
            Some(&crate::ams::Extra::Int(7))
        );
    }

    #[test]
    fn the_content_user_hint_follows_a_copied_uri_only() {
        let mut target = with(|t| t.content_user_hint = USER_CURRENT);
        let other = with(|o| {
            o.type_ = Some("text/plain".into());
            o.content_user_hint = 10;
        });
        fill_in_value(&mut target, &other, 0).expect("merge");
        assert_eq!(facts(&target).content_user_hint, 10);

        // No Uri is copied by an action-only fill-in, so the hint stays.
        let mut target = with(|t| t.content_user_hint = USER_CURRENT);
        let other = with(|o| {
            o.action = Some("fill".into());
            o.content_user_hint = 10;
        });
        fill_in_value(&mut target, &other, 0).expect("merge");
        assert_eq!(facts(&target).content_user_hint, USER_CURRENT);

        // A target with its own hint keeps it.
        let mut target = with(|t| t.content_user_hint = 3);
        let other = with(|o| {
            o.type_ = Some("text/plain".into());
            o.content_user_hint = 10;
        });
        fill_in_value(&mut target, &other, 0).expect("merge");
        assert_eq!(facts(&target).content_user_hint, 3);
    }
}
