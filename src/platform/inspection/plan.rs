//! Source dependencies are decided before collection, never by read success.
use spool_shared_types::inspection::{ReadMode, ReadRequest, Resource, Selection};

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent source dependencies are a set, not mutually exclusive states"
)]
pub(super) struct Plan {
    pub cg: bool,
    pub ax: bool,
    pub apps: bool,
    pub displays: bool,
    pub spaces: bool,
    pub membership: bool,
    pub active: bool,
}
impl Plan {
    pub fn space_field(
        &self,
        request: &ReadRequest,
        selection: &Selection,
        id: Option<u64>,
        scope: &str,
    ) -> bool {
        if request.resource != Resource::Space {
            let space_summary = matches!(request.resource, Resource::Display | Resource::Session)
                && selection.wants("spaces");
            return space_summary
                || scope == "identity.display_id"
                || self.active && scope == "state.visible";
        }
        if matches!(request.mode, ReadMode::Inspect { id: Some(target) } if id != Some(target)) {
            return false;
        }
        matches!(request.mode, ReadMode::List)
            || selection.wants(scope)
            || request.filters.iter().any(|filter| {
                matches!(
                    (filter.field.as_str(), scope),
                    ("display", "identity.display_id")
                        | ("kind", "identity.kind")
                        | ("visible", "state.visible")
                )
            })
    }

    pub fn new(request: &ReadRequest, selection: &Selection) -> Self {
        let list = matches!(request.mode, ReadMode::List);
        let wants = |group: &str| {
            selection
                .paths()
                .any(|path| path == group || path.starts_with(&format!("{group}.")))
        };
        let filter = |field: &str| {
            request
                .filters
                .iter()
                .any(|predicate| predicate.field == field)
        };
        let windows = match request.resource {
            Resource::Window => true,
            Resource::App | Resource::Space | Resource::Session => wants("windows"),
            _ => false,
        };
        let ax = match request.resource {
            Resource::Window => {
                list || wants("identity")
                    || wants("ax")
                    || wants("actions")
                    || wants("parameterized-attributes")
                    || filter("pid")
                    || filter("title")
                    || filter("minimized")
                    || filter("bundle-id")
            }
            _ => windows,
        };
        let cg = request.resource == Resource::Window
            || windows
                && (request.resource != Resource::Window
                    || list
                    || wants("identity")
                    || wants("cg")
                    || filter("pid")
                    || filter("title")
                    || filter("on-screen")
                    || filter("display"));
        let membership = (request.resource == Resource::Window
            && (list || wants("spaces") || filter("space") || filter("display")))
            || (request.resource == Resource::Space && (list || wants("windows")))
            || (request.resource == Resource::Session && wants("windows"));
        let spaces = membership
            || request.resource == Resource::Space
            || (request.resource == Resource::Display && wants("spaces"))
            || (request.resource == Resource::Session && (wants("spaces") || wants("active")));
        let displays = request.resource == Resource::Display
            || spaces
                && (request.resource != Resource::Space
                    || list
                    || wants("identity")
                    || filter("display"))
            || filter("display")
            || (request.resource == Resource::Session && (wants("displays") || wants("active")));
        let apps = ax
            || request.resource == Resource::App
            || (request.resource == Resource::Session && (wants("apps") || wants("active")));
        Self {
            cg,
            ax,
            apps,
            displays,
            spaces,
            membership,
            active: request.resource == Resource::Session && wants("active"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use spool_shared_types::inspection::Source;
    #[test]
    fn cg_only_never_acquires_ax_for_association() {
        let mut r = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
        r.show = vec!["cg".into()];
        let p = Plan::new(&r, &Selection::new(r.resource, r.source, &r.show).unwrap());
        assert!(p.cg);
        assert!(!p.ax);
        assert!(!p.apps);
        assert!(!p.spaces);
    }
    #[test]
    fn geometry_leaf_does_not_enumerate_windows_or_spaces() {
        let mut r = ReadRequest::detail(Resource::Display, Source::Native, Some(1));
        r.show = vec!["geometry.bounds".into()];
        let p = Plan::new(&r, &Selection::new(r.resource, r.source, &r.show).unwrap());
        assert!(p.displays);
        assert!(!p.cg && !p.ax && !p.spaces);
    }
    #[test]
    fn space_state_only_has_no_optional_identity_or_display_dependency() {
        let mut r = ReadRequest::detail(Resource::Space, Source::Native, Some(42));
        r.show = vec!["state".into()];
        let selection = Selection::new(r.resource, r.source, &r.show).unwrap();
        let p = Plan::new(&r, &selection);
        assert!(!p.displays);
        assert!(p.space_field(&r, &selection, Some(42), "state.visible"));
        assert!(!p.space_field(&r, &selection, Some(42), "identity.kind"));
        assert!(!p.space_field(&r, &selection, Some(42), "identity.display_id"));
        assert!(!p.space_field(&r, &selection, Some(99), "state.visible"));
    }
    #[test]
    fn nested_spaces_keep_their_summary_fields() {
        for resource in [Resource::Session, Resource::Display] {
            let mut request = ReadRequest::detail(resource, Source::Native, Some(1));
            request.show = vec!["spaces".into()];
            let selection = Selection::new(resource, Source::Native, &request.show).unwrap();
            let plan = Plan::new(&request, &selection);
            for scope in [
                "identity.ordinal",
                "identity.kind",
                "identity.display_id",
                "state.visible",
            ] {
                assert!(plan.space_field(&request, &selection, Some(42), scope));
            }
        }
    }
}
