//! Resolver chain: exact -> aliases -> history/dictionary -> cache -> LLM.
//!
//! Invariants:
//! - `$0` first: any local/cache hit must prevent an LLM request.
//! - An LLM result is always persisted to the cache before being shown.
//! - A stage error degrades to the next stage; it never crashes the app.
//! - LLM stages only run on an explicit trigger or a full L1 miss.
//!
//! v2 seam: add a stage by implementing [`CommandResolver`] and registering it
//! in [`ResolverChain::build`]; no core changes.

pub mod cache;
pub mod exact;
pub mod llm;
pub mod local;

use crate::aliases::AliasTable;
use crate::config::{Config, ResolverSettings};
use crate::contracts::{Candidate, CommandResolver, ResolveError, ResolveRequest, Trigger};
use std::collections::HashMap;

pub struct ResolverChain {
    stages: Vec<Box<dyn CommandResolver>>,
    max_candidates: usize,
}

impl ResolverChain {
    /// Build in fixed order: exact (10), local (20), llm (90).
    /// LLM is registered but short-circuits itself when disabled/untriggered.
    pub fn build(settings: &ResolverSettings, config: &Config) -> Result<Self, ResolveError> {
        let aliases = AliasTable::load(&settings.aliases_rc, &settings.user_aliases)
            .map_err(|e| ResolveError::Unavailable(e.to_string()))?;
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let exact = exact::ExactResolver::new(aliases, path_dirs);
        let local = local::LocalResolver::new(settings.clone())
            .map_err(|e| ResolveError::Unavailable(e.to_string()))?;
        let cache = cache::ResolveCache::open_with_migration(
            &config.cache_path(),
            &cache::ResolveCache::legacy_cache_path(),
        )
        .map_err(|e| ResolveError::Unavailable(e.to_string()))?;
        let llm = llm::LlmResolver::new(config.llm.clone(), cache)
            .with_max_cache_entries(config.cache.max_entries);
        Ok(Self::from_stages(
            vec![Box::new(exact), Box::new(local), Box::new(llm)],
            settings.max_candidates,
        ))
    }

    pub fn from_stages(mut stages: Vec<Box<dyn CommandResolver>>, max_candidates: usize) -> Self {
        stages.sort_by_key(|s| s.priority());
        Self {
            stages,
            max_candidates: max_candidates.max(1),
        }
    }

    /// Run stages in priority order, merge, dedup by rendered command, cap the list.
    /// Later stages are skipped as soon as `min_results` local candidates exist
    /// unless `req.trigger == Trigger::Explicit`.
    pub fn resolve(&mut self, req: &ResolveRequest) -> Result<Vec<Candidate>, ResolveError> {
        let mut collected = Vec::new();
        let mut last_err: Option<ResolveError> = None;

        for stage in &mut self.stages {
            if stage.priority() >= 90 && req.trigger != Trigger::Explicit && !collected.is_empty() {
                continue;
            }
            match stage.resolve(req) {
                Ok(mut cands) => collected.append(&mut cands),
                Err(err) => last_err = Some(err),
            }
        }

        if collected.is_empty() {
            if let Some(err) = last_err {
                return Err(err);
            }
        }
        Ok(self.merge_with_explicit(collected, req.trigger == Trigger::Explicit))
    }

    #[allow(dead_code)] // Shortcut: цепочка собирается статически, билдер — для тестов.
    pub fn push(&mut self, stage: Box<dyn CommandResolver>) {
        self.stages.push(stage);
        self.stages.sort_by_key(|s| s.priority());
    }

    pub fn clear_cache(&mut self) -> usize {
        self.stages.iter_mut().map(|s| s.clear_cache()).sum()
    }

    /// `$0` live-typing suggestions across local stages + cache (never calls LLM network).
    pub fn resolve_live(&mut self, req: &ResolveRequest) -> Vec<Candidate> {
        let raw = req.raw_input.trim();
        let cleaned = raw.strip_prefix("??").unwrap_or(raw).trim();
        if cleaned.len() < 2 {
            return Vec::new();
        }

        let mut collected = Vec::new();
        for stage in &mut self.stages {
            let mut extra = stage.resolve_live(req);
            collected.append(&mut extra);
        }

        // Drop candidates identical to what is already completely typed, unless it's an alias.
        collected
            .retain(|c| c.rendered != raw || c.source == crate::contracts::CandidateSource::Alias);
        let mut merged = self.merge(collected);
        merged.truncate(req.limit.min(self.max_candidates).min(8));
        merged
    }

    /// Exact-layer shortcut used before opening the overlay.
    pub fn exact_hit(&mut self, req: &ResolveRequest) -> Option<Candidate> {
        for stage in &mut self.stages {
            if stage.priority() > 10 {
                break;
            }
            if let Ok(cands) = stage.resolve(req) {
                if let Some(hit) = cands.into_iter().find(|c| c.source.may_execute_directly()) {
                    return Some(hit);
                }
            }
        }
        None
    }

    /// Dedup + stable sort: safety, then source trust, then score descending.
    pub fn merge(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        self.merge_with_explicit(candidates, false)
    }

    fn merge_with_explicit(&self, candidates: Vec<Candidate>, explicit: bool) -> Vec<Candidate> {
        let rank = |s: crate::contracts::CandidateSource| -> u8 {
            use crate::contracts::CandidateSource::*;
            if explicit {
                match s {
                    Exact => 0,
                    Alias => 1,
                    Builtin => 2,
                    Cache => 3,
                    Llm => 4,
                    History => 5,
                    Dictionary => 6,
                }
            } else {
                match s {
                    Exact => 0,
                    Alias => 1,
                    Builtin => 2,
                    History => 3,
                    Dictionary => 4,
                    Cache => 5,
                    Llm => 6,
                }
            }
        };
        let mut by_rendered: HashMap<String, Candidate> = HashMap::new();
        for cand in candidates {
            by_rendered
                .entry(cand.rendered.clone())
                .and_modify(|existing| {
                    let better = (cand.safety.level, rank(cand.source))
                        < (existing.safety.level, rank(existing.source))
                        || ((cand.safety.level, rank(cand.source))
                            == (existing.safety.level, rank(existing.source))
                            && cand.score > existing.score);
                    if better {
                        *existing = cand.clone();
                    }
                })
                .or_insert(cand);
        }

        let mut merged: Vec<Candidate> = by_rendered.into_values().collect();
        merged.sort_by(|a, b| {
            a.safety
                .level
                .cmp(&b.safety.level)
                .then_with(|| rank(a.source).cmp(&rank(b.source)))
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        merged.truncate(self.max_candidates);
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{CandidateSource, SafetyReport};

    #[test]
    fn explicit_trigger_ranks_llm_ahead_of_history() {
        let chain = ResolverChain::from_stages(Vec::new(), 5);
        let hist = Candidate {
            id: 1,
            template: "python3 -c html".into(),
            rendered: "python3 -c html".into(),
            source: CandidateSource::History,
            score: 5.0,
            safety: SafetyReport::default(),
            note: None,
        };
        let llm = Candidate {
            id: 2,
            template: "mv ~/Documents/*.html ~/Downloads/".into(),
            rendered: "mv ~/Documents/*.html ~/Downloads/".into(),
            source: CandidateSource::Llm,
            score: 0.9,
            safety: SafetyReport::default(),
            note: None,
        };
        let out = chain.merge_with_explicit(vec![hist, llm], true);
        assert_eq!(out[0].source, CandidateSource::Llm);
    }
}
