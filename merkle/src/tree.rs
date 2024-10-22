use sha2::{Digest, Sha256};
pub type Hash = [u8; 32];
pub type Level = Vec<Hash>;

#[derive(Default, Clone)]
pub struct Tree {
    root: Option<Hash>,
    levels: Vec<Level>,
}

impl Tree {
    fn build_next_level(hashes: &[Hash]) -> Level {
        let mut tree_level = Vec::new();
        for i in (0..hashes.len()).step_by(2) {
            let h1 = &hashes[i];
            let h2 = if i + 1 < hashes.len() {
                &hashes[i + 1]
            } else {
                h1
            };
            let mut combined = Vec::from_iter(h1.iter().copied());
            combined.extend(h2.iter());

            let digest = Sha256::digest(&combined);
            let hash: Hash = digest.into();

            tree_level.push(hash);
        }
        tree_level
    }

    /// Get the proof for a leaf node
    ///
    /// The proof is a list of tuples containing the sibling hash and a boolean indicating if the sibling is a left node
    pub fn get_proof(&self, index: usize) -> Vec<(Hash, u8)> {
        let mut proof = Vec::new();
        let mut idx = index;
        for level in &self.levels[..self.levels.len() - 1] {
            let is_left_node = idx % 2 == 0;
            let pair_idx = if is_left_node { idx + 1 } else { idx - 1 };

            match pair_idx.cmp(&level.len()) {
                std::cmp::Ordering::Less => {
                    proof.push((level[pair_idx], is_left_node as u8))
                }
                std::cmp::Ordering::Equal => {
                    proof.push((level[pair_idx - 1], 0))
                }
                _ => panic!("Invalid index"),
            }

            idx /= 2;
        }
        proof
    }

    pub fn root_hash(&self) -> Option<Hash> {
        self.root
    }

    pub fn verify_proof(
        leaf: &Hash,
        proof: &Vec<(Hash, u8)>,
        root: &Hash,
    ) -> bool {
        let mut hash = *leaf;
        for (p, is_left_leave) in proof {
            let mut combined: [u8; 64] = if *is_left_leave > 0 {
                let mut arr = [0; 64];
                arr[..32].copy_from_slice(&hash);
                arr
            } else {
                let mut arr = [0; 64];
                arr[..32].copy_from_slice(p);
                arr
            };

            if *is_left_leave > 0 {
                combined[32..].copy_from_slice(p);
            } else {
                combined[32..].copy_from_slice(&hash);
            }

            hash = Sha256::digest(combined).into();
        }

        hash == *root
    }

    pub fn build_from_leaves(leaves: Level) -> Tree {
        if leaves.is_empty() {
            return Tree::default();
        }

        let mut levels = vec![leaves];
        while levels.last().unwrap().len() > 1 {
            let next_level = Tree::build_next_level(levels.last().unwrap());
            levels.push(next_level);
        }
        let root = levels.last().unwrap()[0];

        Tree {
            root: Some(root),
            levels,
        }
    }

    /// Returns the number of leaves in the tree
    pub fn leaves_count(&self) -> usize {
        if let Some(leaves) = self.levels.first() {
            leaves.len()
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{self, RngCore};

    /// Tests merkle paths verification for trees with leaves count from 1 to 100
    #[test]
    fn test_verify_proof() {
        for leaves_count in 1..100 {
            // Generate random hashes
            let leaves: Vec<Hash> = (0..leaves_count)
                .map(|_| {
                    let mut data = [0u8; 32];
                    rand::thread_rng().fill_bytes(&mut data[..]);
                    data
                })
                .collect();

            let mt = Tree::build_from_leaves(leaves.clone());
            let root = mt.root_hash().expect("valid root");

            // Verify all proofs
            for (i, _) in leaves.iter().enumerate() {
                let proof = mt.get_proof(i);
                assert!(
                    Tree::verify_proof(&leaves[i], &proof, &root),
                    "{}",
                    format!("Failed for index: {}", i)
                );
            }

            // Test invalid proof
            let mut random_hash = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut random_hash[..]);
            assert!(!Tree::verify_proof(&random_hash, &mt.get_proof(0), &root));
        }

        // Test empty tree
        let mt = Tree::build_from_leaves(vec![]);
        assert!(mt.root_hash().is_none());
    }
}
