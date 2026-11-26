//! Process-local rotation for the explicitly configured members of a provider route.
use crate::{
    domain::Route,
    key_pool_config::alternates,
    network_authorization::{NetworkError, NetworkErrorKind},
    provider_transport::{NetworkPurpose, validate_target},
    secrets,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};

const DEFAULT_COOLDOWN: Duration = Duration::from_millis(250);
type Member = [u8; 32];
type PoolId = (String, String);

#[derive(Default)]
pub(crate) struct KeyPool {
    routes: Mutex<HashMap<PoolId, Rotation>>,
}

#[derive(Default)]
struct Rotation {
    last: Option<Member>,
    members: HashMap<Member, Option<Cooldown>>,
}

struct Cooldown {
    since: Instant,
    duration: Duration,
}

impl Cooldown {
    fn remaining(&self, now: Instant) -> Duration {
        self.duration
            .saturating_sub(now.saturating_duration_since(self.since))
    }
}

pub(crate) struct SelectedKey {
    pub(crate) credential: String,
    pub(crate) pooled: bool,
    pool_id: PoolId,
    member: Member,
}

fn credentials(route: &Route, primary: &str) -> Result<Vec<String>, NetworkError> {
    let mut keys = vec![primary.to_owned()];
    keys.extend(alternates(&route.id, &route.base_url)?);
    Ok(keys)
}

/// Looks up membership without advancing the cursor used for dispatch.
pub(crate) fn contains_member(route: &Route, primary: &str, candidate: &str) -> bool {
    primary == candidate
        || alternates(&route.id, &route.base_url)
            .is_ok_and(|alts| alts.iter().any(|k| k == candidate))
}

pub(crate) fn credential_set(
    route: &Route,
    primary: &str,
) -> Result<secrets::CredentialSet, NetworkError> {
    Ok(secrets::CredentialSet::new(credentials(route, primary)?))
}

pub(crate) fn scrub(route: &Route, primary: &str, text: &str) -> Result<String, NetworkError> {
    Ok(credential_set(route, primary)?.scrub(text))
}

impl KeyPool {
    pub(crate) fn select(
        &self,
        route: &Route,
        primary: &str,
        previous: Option<&SelectedKey>,
    ) -> Result<SelectedKey, NetworkError> {
        let keys = credentials(route, primary)?;
        let target = validate_target(&route.base_url, NetworkPurpose::Completion)
            .map_err(|_| NetworkError::not_sent(NetworkErrorKind::Invalid))?;
        self.select_at(
            (route.id.clone(), target.base_url().to_owned()),
            keys,
            Instant::now(),
            previous.map(|key| key.member),
        )
    }

    fn select_at(
        &self,
        pool_id: PoolId,
        keys: Vec<String>,
        now: Instant,
        exclude: Option<Member>,
    ) -> Result<SelectedKey, NetworkError> {
        let mut seen = HashSet::new();
        let candidates: Vec<_> = keys
            .into_iter()
            .filter_map(|key| {
                let member: Member = Sha256::digest(key.as_bytes()).into();
                seen.insert(member).then_some((member, key))
            })
            .collect();
        let pooled = candidates.len() > 1;
        let mut routes = self.routes.lock().unwrap();
        let rotation = routes.entry(pool_id.clone()).or_default();
        rotation.members.retain(|member, _| seen.contains(member));
        for member in seen {
            rotation.members.entry(member).or_default();
        }
        let start = rotation
            .last
            .and_then(|prev| candidates.iter().position(|(member, _)| *member == prev))
            .map_or(0, |i| i + 1);
        let index = (0..candidates.len())
            .map(|i| (start + i) % candidates.len())
            .find(|&i| {
                let member = &candidates[i].0;
                Some(*member) != exclude && (!pooled || ready(rotation.members.get(member), now))
            })
            .ok_or_else(|| NetworkError::not_sent(NetworkErrorKind::Upstream))?;
        let (member, credential) = candidates.into_iter().nth(index).unwrap();
        if pooled {
            rotation.last = Some(member);
        } else {
            routes.remove(&pool_id);
        }
        Ok(SelectedKey {
            credential,
            pooled,
            pool_id,
            member,
        })
    }

    pub(crate) fn is_ready(&self, key: &SelectedKey) -> bool {
        if !key.pooled {
            return true;
        }
        self.routes
            .lock()
            .unwrap()
            .get(&key.pool_id)
            .is_some_and(|rotation| ready(rotation.members.get(&key.member), Instant::now()))
    }

    /// Reports whether a different member can immediately handle the next admitted attempt.
    pub(crate) fn rate_limited(&self, key: &SelectedKey, retry_after: Option<Duration>) -> bool {
        self.rate_limited_at(key, retry_after, Instant::now())
    }

    fn rate_limited_at(
        &self,
        key: &SelectedKey,
        retry_after: Option<Duration>,
        now: Instant,
    ) -> bool {
        if !key.pooled {
            return false;
        }
        let mut routes = self.routes.lock().unwrap();
        let Some(rotation) = routes.get_mut(&key.pool_id) else {
            return false;
        };
        if let Some(slot) = rotation.members.get_mut(&key.member) {
            let duration = retry_after.unwrap_or(DEFAULT_COOLDOWN);
            if slot
                .as_ref()
                .is_none_or(|old| old.remaining(now) < duration)
            {
                *slot = Some(Cooldown {
                    since: now,
                    duration,
                });
            }
        }
        rotation
            .members
            .iter()
            .any(|(id, cooldown)| *id != key.member && ready(Some(cooldown), now))
    }
}

fn ready(slot: Option<&Option<Cooldown>>, now: Instant) -> bool {
    slot.is_some_and(|cooldown| cooldown.as_ref().is_none_or(|c| c.remaining(now).is_zero()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(pool: &KeyPool, keys: &[&str], now: Instant) -> Result<SelectedKey, NetworkError> {
        pool.select_at(
            ("route".into(), "https://example.test".into()),
            keys.iter().map(|s| (*s).into()).collect(),
            now,
            None,
        )
    }

    #[test]
    fn rotates_unique_members_and_recovers_after_cooldown() {
        let pool = KeyPool::default();
        let now = Instant::now();
        let keys = ["primary", "primary", "backup", "backup"];
        let primary = select(&pool, &keys, now).unwrap();
        assert_eq!(primary.credential, "primary");
        assert!(primary.pooled);
        let backup = select(&pool, &keys, now).unwrap();
        assert_eq!(backup.credential, "backup");
        assert_eq!(select(&pool, &keys, now).unwrap().credential, "primary");
        assert!(pool.rate_limited_at(&primary, Some(Duration::MAX), now));
        assert!(pool.rate_limited_at(&primary, Some(Duration::ZERO), now));
        assert!(!pool.is_ready(&primary));
        assert_eq!(select(&pool, &keys, now).unwrap().credential, "backup");
        assert!(!pool.rate_limited_at(&backup, None, now));
        assert_eq!(
            select(&pool, &keys, now).err(),
            Some(NetworkError::not_sent(NetworkErrorKind::Upstream))
        );
        assert_eq!(
            select(&pool, &keys, now + DEFAULT_COOLDOWN)
                .unwrap()
                .credential,
            "backup"
        );
    }

    #[test]
    fn duplicate_only_and_zero_delay_preserve_availability() {
        let pool = KeyPool::default();
        let now = Instant::now();
        let key = select(&pool, &["x", "x"], now).unwrap();
        assert!(!key.pooled);
        assert!(!pool.rate_limited(&key, Some(Duration::MAX)));
        assert!(pool.is_ready(&key));
        let keys = ["x", "y"];
        let x = select(&pool, &keys, now).unwrap();
        assert_eq!(x.credential, "x");
        assert!(pool.rate_limited_at(&x, Some(Duration::ZERO), now));
        // Another request gets y before the failed request retries. It must still switch to y.
        assert_eq!(select(&pool, &keys, now).unwrap().credential, "y");
        let replacement = pool
            .select_at(
                x.pool_id.clone(),
                vec!["x".into(), "y".into()],
                now,
                Some(x.member),
            )
            .unwrap();
        assert_eq!(replacement.credential, "y");
        assert_eq!(select(&pool, &keys, now).unwrap().credential, "x");
    }

    #[test]
    fn concurrent_selections_share_a_cursor() {
        let pool = KeyPool::default();
        let now = Instant::now();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..6)
                .map(|_| {
                    scope.spawn(|| {
                        let mut counts = [0; 3];
                        for _ in 0..10 {
                            let member = select(&pool, &["a", "b", "c"], now).unwrap();
                            counts[["a", "b", "c"]
                                .iter()
                                .position(|key| *key == member.credential)
                                .unwrap()] += 1;
                        }
                        counts
                    })
                })
                .collect();
            let mut totals = [0; 3];
            for worker in workers {
                for (total, count) in totals.iter_mut().zip(worker.join().unwrap()) {
                    *total += count;
                }
            }
            assert_eq!(totals, [20; 3]);
        });
    }
}
