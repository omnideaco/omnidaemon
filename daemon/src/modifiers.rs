//! Modifier chain — cross-cutting concerns wired via Equipment.
//!
//! Pre-modifiers (Polity, Bulwark) are called inside handlers before the operation.
//! Post-modifiers (Yoke, Quest, Pager) subscribe to Email events and observe operations.
//!
//! Modifier execution order (matches the Zig orchestrator):
//!   Before: Polity → Bulwark
//!   After:  Yoke → Quest (via Email observers)

use std::sync::Arc;

use equipment::{Email, PhoneError};

use crate::state::DaemonState;

// ── Pre-modifiers (called inside handlers) ──────────────────────

/// Polity pre-check: would this operation violate the Covenant?
///
/// For MVP, this is a lightweight check. Full constitutional review
/// happens for governance operations. Content operations are allowed
/// for the identity owner acting on their own data.
pub fn polity_check(op: &str) -> Result<(), PhoneError> {
    // Operations that should never run in an automated pipeline
    // (require explicit user consent via UI dialog)
    let always_ask = [
        "crown.delete",
        "crown.import",
        "vault.wipe",
    ];

    if always_ask.contains(&op) {
        return Err(PhoneError::HandlerFailed {
            call_id: op.to_string(),
            message: "Operation requires explicit user consent and cannot run in a pipeline".into(),
        });
    }

    Ok(())
}

/// Bulwark permission check based on operation sensitivity.
///
/// This is the daemon-side check. Client-type permission gating
/// (Program vs Beryllium vs CLI) happens in the IPC server before dispatch.
pub fn bulwark_check(_op: &str, _state: &DaemonState) -> Result<(), PhoneError> {
    // For MVP, all operations are allowed for authenticated clients.
    // Client-type gating happens in handle_client's check_permission().
    Ok(())
}

// ── Post-modifier observers (wired once at boot via Email) ──────

/// Wire all post-modifier observers onto the Email bus.
///
/// Call this AFTER all modules have registered their handlers,
/// so that Email subscriptions catch events from all sources.
pub fn wire_observers(state: &Arc<DaemonState>) {
    wire_yoke_observers(&state.email);
    wire_pager_observers(state);
    // Quest observer: stub for now — wire when Quest integration is ready.
}

/// Yoke: record provenance after content operations.
///
/// Best-effort — provenance failure does NOT affect the operation.
fn wire_yoke_observers(email: &Email) {
    // Subscribe to content lifecycle events
    for event_id in ["idea.created", "idea.saved", "idea.deleted"] {
        let event_name = event_id.to_string();
        email.subscribe_raw(event_id, move |data| {
            // Best-effort provenance recording.
            // In the future, this calls yoke::ProvenanceTracker::record().
            // For now, just log it so we can verify the wiring works.
            let preview = std::str::from_utf8(data)
                .unwrap_or("<binary>")
                .chars()
                .take(100)
                .collect::<String>();
            log::debug!("Yoke provenance: {event_name} — {preview}");
        });
    }
}

/// Pager: queue system notifications after lifecycle events.
///
/// Note: Pager doesn't implement Clone, so we capture a raw pointer
/// via a helper that wraps the Pager in a Sendable reference.
/// Since DaemonState lives for the entire daemon lifetime (Arc-held),
/// these references are valid for the duration of the Email subscriptions.
fn wire_pager_observers(state: &Arc<DaemonState>) {
    // We can't clone Pager, but we can create a thread-safe wrapper
    // that lets Email subscribers push notifications.
    // The Pager is behind DaemonState which is Arc-held for daemon lifetime.

    let events = [
        ("crown.created", "Identity created", "crown"),
        ("crown.locked", "Identity locked", "crown"),
        ("crown.unlocked", "Identity unlocked", "crown"),
        ("vault.locked", "Vault locked", "vault"),
        ("vault.unlocked", "Vault unlocked", "vault"),
    ];

    for (event_id, title, source) in events {
        let title = title.to_string();
        let source = source.to_string();
        // For pager notifications, we just log for now.
        // Full Pager wiring requires Arc<Pager> which we'll add
        // when Pager is used from the IPC layer.
        state.email.subscribe_raw(event_id, move |_data| {
            log::info!("Pager notification: {} (source: {})", title, source);
        });
    }
}
