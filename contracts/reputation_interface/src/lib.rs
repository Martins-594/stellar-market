#![no_std]
//! Shared cross-contract reputation interface for stellar-market.
//!
//! Other contracts (escrow, dispute) depend on this crate to verify a user's
//! reputation without duplicating reputation logic. The reputation contract is
//! expected to implement [`ReputationVerifier`]; consumers gate their actions
//! by calling it through this trait rather than re-deriving scores/badges.

use soroban_sdk::{contracttype, Address, Env};

/// Achievement badges a user can earn, mirroring the **rating tiers**
/// (`ReputationTier`: Bronze/Silver/Gold/Platinum, thresholded on `average_rating`)
/// exposed by the reputation contract.
///
/// This is *not* the reputation contract's `ScoreBadge` ladder
/// (Rising/Established/Elite), which is thresholded on the raw accumulated
/// `total_score` and has different rungs — see the reputation contract's module
/// docs (issue #1172). An implementor of [`ReputationVerifier::has_badge`] should
/// resolve these against `get_tier`/`get_badges`, not against `get_score_badge`.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Badge {
    Bronze = 1,
    Silver = 2,
    Gold = 3,
    Platinum = 4,
}

/// Cross-contract reputation verification interface.
///
/// Implementors expose a user's numeric reputation score and badge ownership so
/// that gating logic (e.g. requiring a minimum score for high-value escrow
/// creation) lives in one place. A typical on-chain implementor wraps a
/// reputation contract client and forwards each call cross-contract.
pub trait ReputationVerifier {
    /// Returns the user's current reputation score.
    fn get_score(&self, env: &Env, user: Address) -> u32;

    /// Returns whether the user holds the given badge.
    fn has_badge(&self, env: &Env, user: Address, badge: Badge) -> bool;
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    /// A mock verifier downstream contracts can use to test gating logic
    /// without deploying the full reputation contract.
    struct MockVerifier {
        score: u32,
        highest_badge: Option<Badge>,
    }

    impl ReputationVerifier for MockVerifier {
        fn get_score(&self, _env: &Env, _user: Address) -> u32 {
            self.score
        }

        fn has_badge(&self, _env: &Env, _user: Address, badge: Badge) -> bool {
            matches!(self.highest_badge, Some(b) if b >= badge)
        }
    }

    #[test]
    fn mock_verifier_reports_score_and_badges() {
        let env = Env::default();
        let user = Address::generate(&env);
        let verifier = MockVerifier {
            score: 72,
            highest_badge: Some(Badge::Silver),
        };

        assert_eq!(verifier.get_score(&env, user.clone()), 72);
        assert!(verifier.has_badge(&env, user.clone(), Badge::Bronze));
        assert!(verifier.has_badge(&env, user.clone(), Badge::Silver));
        assert!(!verifier.has_badge(&env, user, Badge::Gold));
    }

    #[test]
    fn verifier_without_badges_holds_none() {
        let env = Env::default();
        let user = Address::generate(&env);
        let verifier = MockVerifier {
            score: 0,
            highest_badge: None,
        };

        assert_eq!(verifier.get_score(&env, user.clone()), 0);
        assert!(!verifier.has_badge(&env, user, Badge::Bronze));
    }
}
