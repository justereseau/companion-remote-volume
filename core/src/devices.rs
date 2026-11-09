//! Output device selection, free of OS calls: which device a client means, and which comes next.
//!
//! A client names a device by id or by exact name: ids are opaque and OS-specific, while people
//! configuring buttons think in names.

use serde_json::{json, Value};

use crate::backend::Device;

/// The `outputDevices` list as sent on the wire.
pub fn to_json(devices: &[Device]) -> Value {
    Value::Array(
        devices
            .iter()
            .map(|d| json!({ "id": d.id, "name": d.name, "default": d.default }))
            .collect(),
    )
}

/// The device `target` names: by id, then by exact name. Of several devices with that name, the
/// first one.
pub fn resolve<'a>(devices: &'a [Device], target: &str) -> Option<&'a Device> {
    devices
        .iter()
        .find(|d| d.id == target)
        .or_else(|| devices.iter().find(|d| d.name == target))
}

/// Whether `target` is a name that several devices share, so [`resolve`] had to pick one.
pub fn is_ambiguous(devices: &[Device], target: &str) -> bool {
    !devices.iter().any(|d| d.id == target)
        && devices.iter().filter(|d| d.name == target).count() > 1
}

/// The device after the current default among `targets`, in their order and wrapping around; the
/// first one when the default is not among them. Targets that match nothing (a headset unplugged
/// right now) are skipped. No targets means every device. `None` when nothing is left to pick.
pub fn next<'a>(devices: &'a [Device], targets: &[String]) -> Option<&'a Device> {
    let mut candidates: Vec<&Device> = Vec::new();
    if targets.is_empty() {
        candidates.extend(devices);
    } else {
        for device in targets.iter().filter_map(|t| resolve(devices, t)) {
            if !candidates.iter().any(|c| c.id == device.id) {
                candidates.push(device);
            }
        }
    }
    let after_default = candidates
        .iter()
        .position(|d| d.default)
        .map_or(0, |position| position + 1);
    candidates
        .get(after_default)
        .or_else(|| candidates.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, default: bool) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            default,
        }
    }

    fn list() -> Vec<Device> {
        vec![
            device("a", "Speakers", true),
            device("b", "Headset", false),
            device("c", "HDMI", false),
        ]
    }

    fn targets(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn next_id(devices: &[Device], names: &[&str]) -> Option<String> {
        next(devices, &targets(names)).map(|d| d.id.clone())
    }

    #[test]
    fn the_wire_format_has_the_id_the_name_and_the_default_flag() {
        assert_eq!(
            to_json(&list()[..1]).to_string(),
            r#"[{"id":"a","name":"Speakers","default":true}]"#
        );
    }

    #[test]
    fn a_target_is_an_id_then_an_exact_name() {
        let devices = list();
        assert_eq!(resolve(&devices, "b").unwrap().name, "Headset");
        assert_eq!(resolve(&devices, "HDMI").unwrap().id, "c");
        assert!(resolve(&devices, "headset").is_none(), "names are exact");
        assert!(resolve(&devices, "nope").is_none());
        // A name that happens to be another device's id: the id wins.
        let tricky = [device("x", "y", false), device("y", "z", false)];
        assert_eq!(resolve(&tricky, "y").unwrap().id, "y");
    }

    #[test]
    fn a_shared_name_picks_the_first_device_and_is_reported() {
        let twins = [
            device("1", "USB Audio", false),
            device("2", "USB Audio", false),
            device("3", "HDMI", false),
        ];
        assert_eq!(resolve(&twins, "USB Audio").unwrap().id, "1");
        assert!(is_ambiguous(&twins, "USB Audio"));
        assert!(!is_ambiguous(&twins, "HDMI"));
        assert!(!is_ambiguous(&twins, "1"), "an id is never ambiguous");
        assert!(!is_ambiguous(&twins, "nope"));
    }

    #[test]
    fn cycling_all_devices_goes_round() {
        let mut devices = list();
        assert_eq!(next_id(&devices, &[]).as_deref(), Some("b"));
        devices[0].default = false;
        devices[2].default = true;
        assert_eq!(next_id(&devices, &[]).as_deref(), Some("a"), "wraps around");
    }

    #[test]
    fn cycling_a_subset_toggles_between_its_devices_by_id_or_name() {
        let mut devices = list();
        assert_eq!(next_id(&devices, &["a", "Headset"]).as_deref(), Some("b"));
        devices[0].default = false;
        devices[1].default = true;
        assert_eq!(next_id(&devices, &["a", "Headset"]).as_deref(), Some("a"));
    }

    #[test]
    fn cycling_starts_at_the_first_when_the_default_is_not_in_the_subset() {
        assert_eq!(next_id(&list(), &["c", "b"]).as_deref(), Some("c"));
    }

    #[test]
    fn cycling_skips_unplugged_and_repeated_devices() {
        let devices = list();
        assert_eq!(next_id(&devices, &["gone", "a", "c"]).as_deref(), Some("c"));
        // Only the default is left: it stays, there is nothing else to switch to.
        assert_eq!(next_id(&devices, &["gone", "a"]).as_deref(), Some("a"));
        assert_eq!(
            next_id(&devices, &["a", "Speakers", "b"]).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn cycling_finds_nothing_when_no_device_matches() {
        assert_eq!(next_id(&list(), &["gone"]), None);
        assert_eq!(next_id(&[], &[]), None);
    }
}
