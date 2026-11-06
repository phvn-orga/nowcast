//! The attachment rule of contract section 5, apart from reading the process
//! tree.

use std::path::Path;

use globset::{Glob, GlobMatcher};
use nowcast_traits::ClientKind;
use nowcast_types::ClientKindName;

/// The `[anchor] exe` patterns of every declared client kind, compiled once.
#[derive(Debug, Clone)]
pub struct KindMatcher {
    /// In declaration order, so that two kinds matching one path resolve the
    /// same way every time.
    patterns: Vec<(GlobMatcher, ClientKindName)>,
}

impl KindMatcher {
    /// Compiles the patterns of `kinds`. `*` also matches `/`: a pattern is a
    /// glob on the whole path. A pattern that is not a valid glob is logged
    /// and never matches; the other patterns of its kind still apply.
    pub fn new(kinds: &[ClientKind]) -> Self {
        let mut patterns = Vec::new();
        for kind in kinds {
            for pattern in &kind.exe {
                match Glob::new(pattern) {
                    Ok(glob) => patterns.push((glob.compile_matcher(), kind.name.clone())),
                    Err(error) => tracing::warn!(
                        kind = %kind.name,
                        pattern,
                        %error,
                        "ignoring an invalid executable pattern"
                    ),
                }
            }
        }
        Self { patterns }
    }

    /// The kind whose patterns match `exe`, if any.
    pub fn kind_of(&self, exe: &Path) -> Option<&ClientKindName> {
        self.patterns
            .iter()
            .find(|(glob, _)| glob.is_match(exe))
            .map(|(_, name)| name)
    }

    /// Applies the attachment rule to `chain`, the connected process first and
    /// then its ancestors, `exe` giving each one's executable path when it is
    /// known. Returns the nearest item whose executable matches a kind, with
    /// that kind, whatever the kind; failing that, the first item with no kind.
    /// `None` only for an empty chain.
    ///
    /// The chain is consumed lazily: nothing past the match is read.
    pub fn attach<T>(
        &self,
        chain: impl IntoIterator<Item = T>,
        exe: impl Fn(&T) -> Option<&Path>,
    ) -> Option<(T, Option<ClientKindName>)> {
        let mut first = None;
        for item in chain {
            if let Some(kind) = exe(&item).and_then(|path| self.kind_of(path)) {
                let kind = kind.clone();
                return Some((item, Some(kind)));
            }
            if first.is_none() {
                first = Some(item);
            }
        }
        first.map(|item| (item, None))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn kind(name: &str, exe: &[&str]) -> ClientKind {
        ClientKind {
            name: ClientKindName::new(name).unwrap(),
            description: String::new(),
            exe: exe.iter().map(|e| e.to_string()).collect(),
        }
    }

    struct Subject;

    impl nowcast_conformance::client_kind::AttachmentSubject for Subject {
        fn attach(chain: &[PathBuf], kinds: &[ClientKind]) -> (usize, Option<ClientKindName>) {
            let ((index, _), kind) = KindMatcher::new(kinds)
                .attach(chain.iter().enumerate(), |(_, path)| Some(path.as_path()))
                .expect("the suite never passes an empty chain");
            (index, kind)
        }
    }

    nowcast_conformance::attachment_suite!(Subject);

    #[test]
    fn an_invalid_pattern_is_skipped_and_the_others_still_match() {
        let matcher = KindMatcher::new(&[kind("codex", &["*/bin/[codex", "*/bin/codex"])]);
        assert_eq!(
            matcher
                .kind_of(Path::new("/usr/bin/codex"))
                .map(|k| k.as_str()),
            Some("codex")
        );
    }

    #[test]
    fn an_empty_chain_has_no_anchor() {
        let matcher = KindMatcher::new(&[kind("codex", &["*/bin/codex"])]);
        assert!(
            matcher
                .attach(Vec::<PathBuf>::new(), |p| Some(p.as_path()))
                .is_none()
        );
    }

    #[test]
    fn a_process_with_an_unknown_executable_is_skipped() {
        let matcher = KindMatcher::new(&[kind("codex", &["*/bin/codex"])]);
        let chain = [None, None, Some(PathBuf::from("/usr/bin/codex"))];
        let ((index, _), kind) = matcher
            .attach(chain.iter().enumerate(), |(_, exe)| exe.as_deref())
            .unwrap();
        assert_eq!(
            (index, kind.map(|k| k.to_string())),
            (2, Some("codex".into()))
        );
    }

    #[test]
    fn with_no_kinds_the_first_process_anchors() {
        let matcher = KindMatcher::new(&[]);
        let chain = ["/usr/bin/a", "/usr/bin/b"].map(PathBuf::from);
        let (first, kind) = matcher.attach(&chain, |p| Some(p.as_path())).unwrap();
        assert_eq!((first.as_path(), kind), (Path::new("/usr/bin/a"), None));
    }
}
