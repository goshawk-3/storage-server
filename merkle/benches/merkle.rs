// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) DUSK NETWORK. All rights reserved.

use criterion::{criterion_group, criterion_main, Criterion};

use merkle::tree::{Hash, Tree};
use rand::RngCore;

fn build_merkle(c: &mut Criterion) {
    let leaves: Vec<Hash> = (0..10_000)
        .map(|_| {
            let mut data = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut data[..]);
            data
        })
        .collect();

    let label: String = format!("build_merkle_{}", leaves.len());

    c.bench_function(&label, |b| {
        b.iter(|| {
            let mt = Tree::build_from_leaves(leaves.clone());

            let root = mt.root_hash().expect("valid root");
            let leaf = &leaves[leaves.len() - 1];
            let proof = mt.get_proof(leaves.len() - 1);
            assert!(Tree::verify_proof(leaf, &proof, &root));
        })
    });
}

criterion_group!(benches, build_merkle);
criterion_main!(benches);
