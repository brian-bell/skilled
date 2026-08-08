//! Deterministic variant selection and OpenCode effective resolution.
//!
//! Everything here is pure. Selection decides which registered variant an agent
//! would resolve a name to, over catalogs the registry already holds; OpenCode
//! resolution decides what OpenCode would load for a name, over observations the
//! scanner already took. Neither reads the filesystem, and neither invents a
//! verdict for a root that was not read: an unread or unresolvable sighting
//! yields [`OpenCodeResolution::Incomplete`] rather than a guess.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    AgentKind,
    agents::adapter,
    source::{
        CatalogClassification, CatalogProposal, Compatibility, RegisteredSource, SkillCandidate,
    },
};

/// The identity of one registered skill variant.
///
/// Spec 6.4 identifies a variant by source repository, catalog root, skill
/// directory, and compatibility set. All four are carried, which is what keeps
/// a Claude Code edition and a Codex edition of one name independent even
/// though the displayed skill identity is only the name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantRef {
    source_id: i64,
    source_label: String,
    catalog_relative_path: PathBuf,
    variant_relative_path: PathBuf,
    skill_name: String,
    classification: CatalogClassification,
    compatibility: Compatibility,
}

impl VariantRef {
    pub(crate) fn of(
        source: &RegisteredSource,
        catalog: &CatalogProposal,
        candidate: &SkillCandidate,
    ) -> Self {
        Self {
            source_id: source.id(),
            source_label: source.label().to_owned(),
            catalog_relative_path: catalog.relative_path().to_path_buf(),
            variant_relative_path: candidate.relative_path().to_path_buf(),
            skill_name: candidate.directory_name().to_owned(),
            classification: catalog.classification(),
            compatibility: catalog.compatibility(),
        }
    }

    /// The name an agent would load this variant under.
    ///
    /// Carried from the candidate's own directory name rather than recovered
    /// from the tail of [`Self::variant_relative_path`]: a path component that
    /// is not valid UTF-8 has no `&str` to hand back, and a name is not
    /// something an installation may guess at.
    pub fn skill_name(&self) -> &str {
        &self.skill_name
    }

    /// The registered source's stable identity.
    ///
    /// Labels come from the checkout's directory name, so two repositories can
    /// share one. Anything deciding whether two variants came from the same
    /// source must compare this.
    pub fn source_id(&self) -> i64 {
        self.source_id
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn catalog_relative_path(&self) -> &Path {
        &self.catalog_relative_path
    }

    pub fn variant_relative_path(&self) -> &Path {
        &self.variant_relative_path
    }

    /// The total order every list of variants is reported in.
    ///
    /// Deterministic and independent of the order the registry hands its
    /// sources over, so the same registry always yields the same answer and the
    /// same evidence order. The label leads because it is what a reader
    /// recognises; the id follows it because two checkouts can share a label.
    fn order_key(&self) -> (&str, i64, &Path, &Path) {
        (
            &self.source_label,
            self.source_id,
            &self.catalog_relative_path,
            &self.variant_relative_path,
        )
    }

    /// Whether this variant is one the agent could actually use.
    ///
    /// Two things have to hold, and the stored compatibility set alone is not
    /// enough for either. It must cover the agent — that part is the user's own
    /// declaration, made when the catalog was confirmed. And, where the variant
    /// is an agent-specific edition, it must be *this* agent's edition.
    ///
    /// The second condition exists because the compatibility a catalog is
    /// proposed with is derived from which agents would *find* variants there,
    /// and OpenCode finds them in Claude Code's and Codex's roots. A catalog
    /// laid out as `.claude/skills` is therefore proposed as OpenCode-compatible
    /// while holding the Claude Code edition. Reading that flag as usability
    /// would have Skilled resolve OpenCode to another agent's edition and call
    /// it healthy — which is the conflation spec 5.1 exists to separate.
    ///
    /// A catalog no agent owns — one the user classified agent-specific whose
    /// path names no agent — has no path evidence to refine the declaration
    /// with, so the stored set stands alone.
    pub(crate) fn usable_by(&self, agent: AgentKind) -> bool {
        if !self.compatibility.supports(agent) {
            return false;
        }
        if self.classification == CatalogClassification::Common {
            return true;
        }
        let owner = AgentKind::ALL
            .into_iter()
            .find(|kind| adapter(*kind).owns_source_catalog(&self.catalog_relative_path));
        owner.is_none_or(|owner| owner == agent)
    }

    /// One phrase naming this variant, for a finding's evidence.
    pub fn evidence_label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.source_label,
            self.catalog_relative_path.display(),
            self.variant_relative_path.display()
        )
    }
}

/// What the per-agent selection rule settled on for one skill name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateSelection {
    /// Exactly one variant survives, so the agent's variant is unambiguous.
    Selected(VariantRef),
    /// More than one survives. Spec 6.4 calls this a duplicate conflict, and
    /// installation stays blocked until the user picks a source.
    Duplicate(Vec<VariantRef>),
    /// No registered variant of that name is compatible with the agent.
    NoCandidate,
}

/// Apply the spec 6.4 selection order for one agent and one skill name.
///
/// The order is `exact agent-specific variant → compatible common variant →
/// incompatible`: incompatible variants are dropped outright, and an
/// agent-specific survivor excludes every common one, so a repository shipping
/// both a common and an agent-specific edition is not a conflict.
///
/// "Exact" is read from two places, because no single stored field carries it.
/// A catalog's classification records that it is agent-specific but not which
/// agent it is specific to, and its compatibility set records which agents were
/// confirmed for it — a set that, for a catalog laid out under another agent's
/// root, includes every agent that merely reads that root. Where the catalog's
/// path names an owner, that owner is what "exact" means and the stored set
/// narrows no further; see [`VariantRef::usable_by`], which is what this
/// applies. A user who ticks a second agent on such a catalog will find it
/// selects nothing for them, and Skilled states no reason for it: telling a
/// path-derived tick from a hand-set one would need the two recorded apart,
/// which the store does not do today.
///
/// Two exclusions are worth stating because neither is visible in the rule.
/// A variant that does not validate is not a candidate — an agent could not
/// load it, so it cannot be what an agent resolves to. And a source or catalog
/// that could not be read contributes nothing: this understates what is
/// registered rather than inventing candidates, so a duplicate may go unstated
/// where a checkout is unavailable, and none is ever stated falsely.
pub fn select_candidates(
    sources: &[RegisteredSource],
    agent: AgentKind,
    skill_name: &str,
) -> CandidateSelection {
    let variants: Vec<VariantRef> = readable_variants(sources)
        .filter(|(name, _)| *name == skill_name)
        .map(|(_, variant)| variant)
        .collect();
    narrow(&variants, agent)
}

/// Every readable, valid registered variant, with the name it installs under.
///
/// The exclusions live here rather than in the caller so every path through
/// selection applies the same ones.
fn readable_variants(
    sources: &[RegisteredSource],
) -> impl Iterator<Item = (&str, VariantRef)> + '_ {
    sources
        .iter()
        .filter(|source| source.source_error().is_none())
        .flat_map(|source| {
            source
                .catalogs()
                .iter()
                .filter(|catalog| catalog.included() && catalog.scan_error().is_none())
                .flat_map(move |catalog| {
                    catalog
                        .candidates()
                        .iter()
                        .filter(|candidate| candidate.validation().is_valid())
                        .map(move |candidate| {
                            (
                                candidate.directory_name(),
                                VariantRef::of(source, catalog, candidate),
                            )
                        })
                })
        })
}

/// Every registered variant of every name, gathered in one pass.
///
/// The registry is walked once and narrowed per agent afterwards, because a
/// scan that asked [`select_candidates`] for each name and agent separately
/// would re-walk every catalog of every source for each of them.
pub(crate) fn variants_by_name(sources: &[RegisteredSource]) -> BTreeMap<&str, Vec<VariantRef>> {
    let mut by_name: BTreeMap<&str, Vec<VariantRef>> = BTreeMap::new();
    for (name, variant) in readable_variants(sources) {
        by_name.entry(name).or_default().push(variant);
    }
    by_name
}

/// Apply the selection order to the variants of one name for one agent.
pub(crate) fn narrow(variants: &[VariantRef], agent: AgentKind) -> CandidateSelection {
    let mut candidates: Vec<VariantRef> = variants
        .iter()
        .filter(|variant| variant.usable_by(agent))
        .cloned()
        .collect();
    candidates.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
    if candidates
        .iter()
        .any(|variant| variant.classification == CatalogClassification::AgentSpecific)
    {
        candidates.retain(|variant| variant.classification == CatalogClassification::AgentSpecific);
    }
    match candidates.len() {
        0 => CandidateSelection::NoCandidate,
        1 => CandidateSelection::Selected(candidates.remove(0)),
        _ => CandidateSelection::Duplicate(candidates),
    }
}

/// The roots OpenCode discovers, in the precedence order it resolves them in.
///
/// Read from [`crate::agents`] rather than restated here: the adapter declares
/// OpenCode's native root and then its compatibility roots in order, and a
/// documentation change therefore moves this order with it. Every root OpenCode
/// consults is another agent's native root, which is why one scan of the three
/// native roots is all the evidence this needs.
pub fn opencode_root_order() -> Vec<AgentKind> {
    opencode_roots().collect()
}

/// The same order, without the allocation a per-row walk would pay for.
fn opencode_roots() -> impl Iterator<Item = AgentKind> {
    let opencode = adapter(AgentKind::OpenCode);
    std::iter::once(opencode.native_skill_root())
        .chain(opencode.compatibility_skill_roots().iter().copied())
        .filter_map(|root| {
            AgentKind::ALL
                .into_iter()
                .find(|kind| adapter(*kind).native_skill_root() == root)
        })
}

/// What one root holds under one skill name, as far as the scan established it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootSighting {
    /// The root was not read: it is not selected, was not scanned, or could not
    /// be read in full.
    Unread,
    /// The root was read and offers nothing OpenCode could load under this
    /// name — nothing is there, or what is there is not a skill directory.
    NothingToLoad,
    /// Something is there whose resolution could not be established, so what
    /// OpenCode would load through this root is not known. The entry itself is
    /// on the row's own observation, which is where the reader is shown it.
    Unknown,
    /// The root offers content that resolves to a directory.
    Offers(SightedEntry),
}

/// One root's resolvable content under a skill name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SightedEntry {
    path: PathBuf,
    canonical: PathBuf,
    variant: Option<VariantRef>,
}

impl SightedEntry {
    pub fn new(path: PathBuf, canonical: PathBuf, variant: Option<VariantRef>) -> Self {
        Self {
            path,
            canonical,
            variant,
        }
    }
}

/// One root's contribution to what OpenCode would load, named by its root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeEntry {
    root: AgentKind,
    path: PathBuf,
    canonical: PathBuf,
    variant: Option<VariantRef>,
}

impl OpenCodeEntry {
    /// The agent whose native root OpenCode reached this through.
    pub fn root(&self) -> AgentKind {
        self.root
    }

    /// The installation slot, as it sits in that root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn variant(&self) -> Option<&VariantRef> {
        self.variant.as_ref()
    }
}

/// What OpenCode would load for one skill name, across the roots it consults.
///
/// Sameness is canonical-path equality, not byte comparison: two physical
/// copies are two variants by the spec 6.4 identity even if their bytes agree
/// today, and the full-directory content hash that could prove byte-sameness is
/// Milestone 3 work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenCodeResolution {
    /// No root OpenCode consults offers this name.
    NothingVisible,
    /// One directory, reached through one root or aliased through several. The
    /// highest-precedence root wins and the rest are recorded against it.
    Selected {
        winner: OpenCodeEntry,
        aliases: Vec<OpenCodeEntry>,
    },
    /// The only definition visible is another agent's agent-specific variant,
    /// reached through a compatibility root. Skilled does not claim OpenCode
    /// usability for it.
    ForeignExposure {
        winner: OpenCodeEntry,
        aliases: Vec<OpenCodeEntry>,
    },
    /// More than one directory is visible under the name. Precedence still
    /// picks one, but the arrangement is a conflicting duplicate.
    Conflict { entries: Vec<OpenCodeEntry> },
    /// Something OpenCode consults could not be established, so no
    /// classification is stated. Each root that left it unknown is named, with
    /// what about it was unknown.
    Incomplete { roots: Vec<UnknownRoot> },
}

/// One root whose contribution to a name could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownRoot {
    root: AgentKind,
    cause: UnknownCause,
}

impl UnknownRoot {
    pub fn root(self) -> AgentKind {
        self.root
    }

    pub fn cause(self) -> UnknownCause {
        self.cause
    }
}

/// Why a root's contribution is unknown.
///
/// The two are not the same answer and are never flattened: a root Skilled
/// never read might hold anything, while a root it read in full holds one entry
/// it could not follow. Saying "not read" of the second would be false.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnknownCause {
    /// The root was not read: not selected, not scanned, or unreadable.
    RootNotRead,
    /// The root was read, but the entry under this name could not be resolved.
    EntryUnresolved,
}

/// Decide what OpenCode would load for one name.
///
/// `sightings` is indexed by [`AgentKind`], one per native root; the precedence
/// walked over them is [`opencode_root_order`], so a caller cannot get the order
/// wrong by assembling its array in the wrong sequence.
///
/// A root left unknown blocks every classification, including one a
/// higher-precedence root would seem to settle: a lower root can hold a
/// different directory under the same name, which is exactly the difference
/// between a selection and a conflict.
pub fn resolve_opencode(sightings: [RootSighting; 3]) -> OpenCodeResolution {
    let mut unknown = Vec::new();
    let mut entries = Vec::new();
    for root in opencode_roots() {
        match &sightings[root.index()] {
            RootSighting::Unread => unknown.push(UnknownRoot {
                root,
                cause: UnknownCause::RootNotRead,
            }),
            RootSighting::Unknown => unknown.push(UnknownRoot {
                root,
                cause: UnknownCause::EntryUnresolved,
            }),
            RootSighting::NothingToLoad => {}
            RootSighting::Offers(entry) => entries.push(OpenCodeEntry {
                root,
                path: entry.path.clone(),
                canonical: entry.canonical.clone(),
                variant: entry.variant.clone(),
            }),
        }
    }
    if !unknown.is_empty() {
        return OpenCodeResolution::Incomplete { roots: unknown };
    }
    let mut entries = entries.into_iter();
    let Some(winner) = entries.next() else {
        return OpenCodeResolution::NothingVisible;
    };
    let aliases: Vec<OpenCodeEntry> = entries.collect();
    if aliases
        .iter()
        .any(|entry| entry.canonical != winner.canonical)
    {
        let mut conflicting = vec![winner];
        conflicting.extend(aliases);
        return OpenCodeResolution::Conflict {
            entries: conflicting,
        };
    }
    // Exposure is decided over every root the name is visible through, not the
    // winner alone: content that resolved to no registered variant cannot have
    // its compatibility checked at all, and one OpenCode-compatible sighting is
    // enough to make the name usable.
    //
    // The root it was reached through is deliberately not part of the test:
    // the claim Skilled must not make — that OpenCode can use this — is exactly
    // as false for another agent's variant sitting in OpenCode's own root, and
    // reporting only the compatibility-root case would put a healthy badge over
    // the worse arrangement. What OpenCode can use is
    // [`VariantRef::usable_by`], which is narrower than the stored
    // compatibility set for the reason recorded there.
    let foreign = std::iter::once(&winner).chain(&aliases).all(|entry| {
        entry
            .variant
            .as_ref()
            .is_some_and(|variant| !variant.usable_by(AgentKind::OpenCode))
    });
    if foreign {
        OpenCodeResolution::ForeignExposure { winner, aliases }
    } else {
        OpenCodeResolution::Selected { winner, aliases }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::source::{
        CatalogClassification, CatalogProposal, InspectedSource, RegisteredSource, SkillCandidate,
    };

    fn catalog(
        relative_path: &str,
        classification: CatalogClassification,
        agents: &[AgentKind],
        names: &[&str],
    ) -> CatalogProposal {
        let mut catalog = CatalogProposal::for_test(
            relative_path,
            names
                .iter()
                .map(|name| SkillCandidate::for_test_in(relative_path, name))
                .collect(),
            None,
        );
        if classification == CatalogClassification::AgentSpecific {
            catalog.toggle_classification();
        }
        for agent in AgentKind::ALL {
            if !agents.contains(&agent) {
                catalog.toggle_compatibility(agent);
            }
        }
        catalog
    }

    fn source(id: i64, label: &str, catalogs: Vec<CatalogProposal>) -> RegisteredSource {
        RegisteredSource::new(
            id,
            label.to_owned(),
            InspectedSource::from_stored(
                PathBuf::from(format!("/fixture/{label}")),
                None,
                "0123456".to_owned(),
                None,
                None,
            ),
            catalogs,
            0,
            None,
        )
    }

    fn labels(variants: &[VariantRef]) -> Vec<String> {
        variants.iter().map(VariantRef::evidence_label).collect()
    }

    /// Spec 6.4: an exact agent-specific variant is preferred to a compatible
    /// common one, so a repository shipping both editions is not a conflict.
    #[test]
    fn an_agent_specific_variant_beats_a_compatible_common_one() {
        let sources = [source(
            1,
            "fixture",
            vec![
                catalog(
                    "skills",
                    CatalogClassification::Common,
                    &AgentKind::ALL,
                    &["review"],
                ),
                catalog(
                    "claude/skills",
                    CatalogClassification::AgentSpecific,
                    &[AgentKind::ClaudeCode],
                    &["review"],
                ),
            ],
        )];

        let CandidateSelection::Selected(variant) =
            select_candidates(&sources, AgentKind::ClaudeCode, "review")
        else {
            panic!("one agent-specific variant survives for Claude Code");
        };
        assert_eq!(variant.catalog_relative_path(), Path::new("claude/skills"));

        // Codex cannot use the Claude Code edition at all, so the common
        // variant is what it resolves to.
        let CandidateSelection::Selected(variant) =
            select_candidates(&sources, AgentKind::Codex, "review")
        else {
            panic!("the common variant survives for Codex");
        };
        assert_eq!(variant.catalog_relative_path(), Path::new("skills"));
    }

    /// A variant whose compatibility set excludes the agent is not a candidate
    /// for it, however it is classified.
    #[test]
    fn an_incompatible_variant_is_never_a_candidate() {
        let sources = [source(
            1,
            "fixture",
            vec![catalog(
                "claude/skills",
                CatalogClassification::AgentSpecific,
                &[AgentKind::ClaudeCode],
                &["review"],
            )],
        )];

        assert_eq!(
            select_candidates(&sources, AgentKind::Codex, "review"),
            CandidateSelection::NoCandidate
        );
    }

    /// Two surviving candidates are a duplicate conflict, and the order they are
    /// reported in comes from the variants themselves rather than from the order
    /// the registry happened to hand them over.
    #[test]
    fn competing_variants_are_a_duplicate_reported_in_a_stable_order() {
        let first = source(
            7,
            "alpha",
            vec![catalog(
                "skills",
                CatalogClassification::Common,
                &AgentKind::ALL,
                &["review"],
            )],
        );
        let second = source(
            2,
            "beta",
            vec![catalog(
                "skills",
                CatalogClassification::Common,
                &AgentKind::ALL,
                &["review"],
            )],
        );

        let forwards = select_candidates(
            &[first.clone(), second.clone()],
            AgentKind::ClaudeCode,
            "review",
        );
        let backwards = select_candidates(&[second, first], AgentKind::ClaudeCode, "review");

        let CandidateSelection::Duplicate(variants) = &forwards else {
            panic!("two competing variants are a duplicate: {forwards:?}");
        };
        assert_eq!(
            labels(variants),
            [
                "alpha · skills · skills/review",
                "beta · skills · skills/review"
            ]
        );
        assert_eq!(forwards, backwards);
    }

    /// A name no registered catalog offers the agent has no candidate at all,
    /// which is not a conflict.
    #[test]
    fn a_name_no_catalog_offers_has_no_candidate() {
        let sources = [source(
            1,
            "fixture",
            vec![catalog(
                "skills",
                CatalogClassification::Common,
                &AgentKind::ALL,
                &["review"],
            )],
        )];

        assert_eq!(
            select_candidates(&sources, AgentKind::ClaudeCode, "absent"),
            CandidateSelection::NoCandidate
        );
    }

    /// Precedence is the adapter's own declaration, read rather than restated:
    /// OpenCode's native root, then its compatibility roots in declared order.
    #[test]
    fn opencode_consults_its_native_root_before_its_compatibility_roots() {
        assert_eq!(
            opencode_root_order(),
            [AgentKind::OpenCode, AgentKind::Codex, AgentKind::ClaudeCode]
        );
    }

    fn entry(path: &str, canonical: &str, variant: Option<VariantRef>) -> RootSighting {
        RootSighting::Offers(SightedEntry::new(
            PathBuf::from(path),
            PathBuf::from(canonical),
            variant,
        ))
    }

    fn variant(
        catalog: &str,
        classification: CatalogClassification,
        agents: &[AgentKind],
    ) -> VariantRef {
        let catalog = self::catalog(catalog, classification, agents, &["review"]);
        VariantRef::of(
            &source(1, "fixture", Vec::new()),
            &catalog,
            &catalog.candidates()[0],
        )
    }

    /// One directory reached through every root is a benign alias: the native
    /// installation wins and the others are recorded against it.
    #[test]
    fn one_directory_seen_through_every_root_is_a_benign_alias() {
        let mut per_agent = [
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
        ];
        per_agent[AgentKind::ClaudeCode.index()] =
            entry("/home/.claude/skills/review", "/repo/skills/review", None);
        per_agent[AgentKind::Codex.index()] =
            entry("/home/.agents/skills/review", "/repo/skills/review", None);
        per_agent[AgentKind::OpenCode.index()] = entry(
            "/home/.config/opencode/skills/review",
            "/repo/skills/review",
            None,
        );

        let OpenCodeResolution::Selected { winner, aliases } = resolve_opencode(per_agent) else {
            panic!("one directory through three roots is one selection");
        };
        assert_eq!(winner.root(), AgentKind::OpenCode);
        assert_eq!(
            aliases.iter().map(OpenCodeEntry::root).collect::<Vec<_>>(),
            [AgentKind::Codex, AgentKind::ClaudeCode]
        );
    }

    /// Two different directories behind one name is a conflicting duplicate,
    /// and every party to it is named in precedence order.
    #[test]
    fn different_directories_behind_one_name_conflict() {
        let mut per_agent = [
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
        ];
        per_agent[AgentKind::ClaudeCode.index()] =
            entry("/home/.claude/skills/review", "/repo/one/review", None);
        per_agent[AgentKind::Codex.index()] =
            entry("/home/.agents/skills/review", "/repo/two/review", None);

        let OpenCodeResolution::Conflict { entries } = resolve_opencode(per_agent) else {
            panic!("two directories behind one name is a conflict");
        };
        assert_eq!(
            entries.iter().map(OpenCodeEntry::root).collect::<Vec<_>>(),
            [AgentKind::Codex, AgentKind::ClaudeCode]
        );
    }

    /// An agent-specific variant of another agent, visible only through a
    /// compatibility root, is exposed rather than usable: Skilled does not claim
    /// OpenCode can load it.
    #[test]
    fn a_foreign_agent_specific_variant_is_exposure_not_a_selection() {
        let mut per_agent = [
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
            RootSighting::NothingToLoad,
        ];
        per_agent[AgentKind::ClaudeCode.index()] = entry(
            "/home/.claude/skills/review",
            "/repo/claude/skills/review",
            Some(variant(
                "claude/skills",
                CatalogClassification::AgentSpecific,
                &[AgentKind::ClaudeCode],
            )),
        );

        assert!(matches!(
            resolve_opencode(per_agent),
            OpenCodeResolution::ForeignExposure { .. }
        ));
    }

    /// The non-cases of the rule above. Compatibility cannot be checked for
    /// content that resolved to no registered variant, and a common variant is
    /// OpenCode-compatible by the set that was stored for it, so neither is
    /// foreign exposure.
    #[test]
    fn unregistered_and_common_content_through_a_compatibility_root_is_not_exposure() {
        for candidate in [
            None,
            Some(variant(
                "skills",
                CatalogClassification::Common,
                &AgentKind::ALL,
            )),
        ] {
            let mut per_agent = [
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
            ];
            per_agent[AgentKind::ClaudeCode.index()] = entry(
                "/home/.claude/skills/review",
                "/repo/skills/review",
                candidate,
            );

            assert!(
                matches!(
                    resolve_opencode(per_agent),
                    OpenCodeResolution::Selected { .. }
                ),
                "content OpenCode may load is a selection, not exposure"
            );
        }
    }

    /// Nothing under the name in any consulted root is nothing to resolve.
    #[test]
    fn a_name_no_consulted_root_holds_resolves_to_nothing() {
        assert_eq!(
            resolve_opencode([
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
            ]),
            OpenCodeResolution::NothingVisible
        );
    }

    /// A root that was not read, and an entry that could not be resolved, each
    /// leave the effective resolution unknown: a lower-precedence root can hold
    /// the very directory that would turn a selection into a conflict, so no
    /// classification may be stated over one that was not fully read.
    #[test]
    fn an_unread_or_unresolvable_root_states_no_classification() {
        for (unknown, cause) in [
            (RootSighting::Unread, UnknownCause::RootNotRead),
            (RootSighting::Unknown, UnknownCause::EntryUnresolved),
        ] {
            let mut per_agent = [
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
                RootSighting::NothingToLoad,
            ];
            per_agent[AgentKind::OpenCode.index()] = entry(
                "/home/.config/opencode/skills/review",
                "/repo/skills/review",
                None,
            );
            per_agent[AgentKind::ClaudeCode.index()] = unknown;

            let OpenCodeResolution::Incomplete { roots } = resolve_opencode(per_agent) else {
                panic!("an unread root cannot be classified over");
            };
            assert_eq!(
                roots.iter().map(|root| root.root()).collect::<Vec<_>>(),
                [AgentKind::ClaudeCode]
            );
            // The two kinds of unknown are never flattened: a root that was
            // never read might hold anything, while a root read in full holds
            // one entry that could not be followed.
            assert_eq!(roots[0].cause(), cause);
        }
    }
}
