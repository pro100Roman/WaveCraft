#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildVersionState {
    pub ci_tag: Option<String>,
    pub head: Option<String>,
    pub short_hash: Option<String>,
    pub branch: Option<String>,
    pub origin_main: Option<String>,
    pub clean: bool,
    pub github_main: bool,
}

pub fn classify_build_version(state: &BuildVersionState) -> String {
    if let Some(tag) = state.ci_tag.as_deref().filter(|tag| !tag.trim().is_empty()) {
        return tag.to_owned();
    }
    let main_branch = state.branch.as_deref() == Some("main") || state.github_main;
    let matches_origin =
        state.head.is_some() && (state.head == state.origin_main || state.github_main);
    if state.clean && main_branch && matches_origin {
        "origin main".to_owned()
    } else {
        format!(
            "dirty · {} · {}",
            state.branch.as_deref().unwrap_or("detached"),
            state.short_hash.as_deref().unwrap_or("unknown")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_main() -> BuildVersionState {
        BuildVersionState {
            head: Some("0123456789abcdef".to_owned()),
            short_hash: Some("01234567".to_owned()),
            branch: Some("main".to_owned()),
            origin_main: Some("0123456789abcdef".to_owned()),
            clean: true,
            ..BuildVersionState::default()
        }
    }

    #[test]
    fn ci_tag_takes_precedence_over_repository_state() {
        let state = BuildVersionState {
            ci_tag: Some("v1.2.3".to_owned()),
            ..BuildVersionState::default()
        };

        assert_eq!(classify_build_version(&state), "v1.2.3");
    }

    #[test]
    fn clean_main_matching_origin_is_identified() {
        assert_eq!(classify_build_version(&clean_main()), "origin main");
    }

    #[test]
    fn github_main_checkout_does_not_require_a_remote_ref() {
        let mut state = clean_main();
        state.branch = None;
        state.origin_main = None;
        state.github_main = true;

        assert_eq!(classify_build_version(&state), "origin main");
    }

    #[test]
    fn changes_or_another_branch_include_the_commit_hash() {
        let mut changed = clean_main();
        changed.clean = false;
        assert_eq!(classify_build_version(&changed), "dirty · main · 01234567");

        let mut branch = clean_main();
        branch.branch = Some("feature/version-label".to_owned());
        assert_eq!(
            classify_build_version(&branch),
            "dirty · feature/version-label · 01234567"
        );

        let mut ahead = clean_main();
        ahead.head = Some("fedcba9876543210".to_owned());
        assert_eq!(classify_build_version(&ahead), "dirty · main · 01234567");
    }

    #[test]
    fn detached_dirty_build_has_an_explicit_branch_placeholder() {
        let state = BuildVersionState {
            short_hash: Some("01234567".to_owned()),
            ..BuildVersionState::default()
        };

        assert_eq!(
            classify_build_version(&state),
            "dirty · detached · 01234567"
        );
    }
}
