// SPDX-License-Identifier: Apache-2.0
//! Host tests for the hash and signature primitives.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use crate::ed25519::{self, testing};
use crate::{Sha256, Sha512, from_hex, sha256, sha512, to_hex};

fn decode(hex: &str) -> Vec<u8> {
    let mut out = vec![0u8; hex.len() / 2];
    assert_eq!(
        from_hex(hex.as_bytes(), &mut out),
        Some(out.len()),
        "bad vector {hex}"
    );
    out
}

fn array<const N: usize>(hex: &str) -> [u8; N] {
    let decoded = decode(hex);
    assert_eq!(decoded.len(), N);
    let mut out = [0u8; N];
    out.copy_from_slice(&decoded);
    out
}

fn hex_string(data: &[u8]) -> String {
    let mut out = vec![0u8; data.len() * 2];
    let written = to_hex(data, &mut out).unwrap();
    String::from_utf8(out[..written].to_vec()).unwrap()
}

#[test]
fn sha256_matches_published_vectors() {
    assert_eq!(
        hex_string(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex_string(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex_string(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn sha256_streams_without_changing_the_digest() {
    let data = vec![0x5au8; 1000];
    let mut hasher = Sha256::new();
    for chunk in data.chunks(7) {
        hasher.update(chunk);
    }
    assert_eq!(hasher.finish(), sha256(&data));
}

#[test]
fn sha512_matches_published_vectors() {
    assert_eq!(
        hex_string(&sha512(b"")),
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
         47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        hex_string(&sha512(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        hex_string(&sha512(
            b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
              ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
        )),
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
         501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
}

#[test]
fn sha512_streams_without_changing_the_digest() {
    let data = vec![0xa5u8; 4096];
    let mut hasher = Sha512::new();
    for chunk in data.chunks(333) {
        hasher.update(chunk);
    }
    assert_eq!(hasher.finish(), sha512(&data));
}

#[test]
fn hex_round_trips_and_rejects_malformed_input() {
    let mut out = [0u8; 8];
    assert_eq!(from_hex(b"00ff10", &mut out), Some(3));
    assert_eq!(&out[..3], &[0x00, 0xff, 0x10]);
    assert_eq!(from_hex(b"0", &mut out), None);
    assert_eq!(from_hex(b"zz", &mut out), None);
    assert_eq!(from_hex(b"0011", &mut [0u8; 1]), None);
    assert_eq!(to_hex(&[0xde, 0xad], &mut [0u8; 3]), None);
}

#[test]
fn curve_constants_satisfy_their_defining_equations() {
    assert!(testing::constants_ok());
    assert_eq!(
        hex_string(&testing::base_encoding()),
        "5866666666666666666666666666666666666666666666666666666666666666"
    );
}

#[test]
fn base_point_has_the_group_order() {
    // [L]B is the identity, and [2]B is not.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    assert_eq!(testing::multiply_base(&testing::ORDER_BYTES), identity);

    let mut two = [0u8; 32];
    two[0] = 2;
    let doubled = testing::multiply_base(&two);
    assert_ne!(doubled, identity);
    assert_ne!(doubled, testing::base_encoding());
}

#[test]
fn scalar_reduction_matches_known_values() {
    // L itself reduces to zero; L + 1 reduces to one.
    let mut order = [0u8; 64];
    for (index, limb) in testing::ORDER.iter().enumerate() {
        order[index * 8..index * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    assert_eq!(testing::reduce(&order), [0u8; 32]);
    order[0] += 1;
    let mut one = [0u8; 32];
    one[0] = 1;
    assert_eq!(testing::reduce(&order), one);

    // A 512-bit all-ones value must reduce to a canonical scalar.
    let reduced = testing::reduce(&[0xffu8; 64]);
    assert!(testing::is_canonical(&reduced));
}

#[test]
fn signatures_verify_and_tampering_is_rejected() {
    let seed = array::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public = ed25519::public_key_from_seed(&seed);
    let message = b"aether update manifest";
    let signature = ed25519::sign(&seed, message);
    assert!(ed25519::verify(&public, message, &signature));
    assert!(!ed25519::verify(
        &public,
        b"aether update manifesX",
        &signature
    ));
    assert!(!ed25519::verify(&public, message, &[0u8; 64]));

    let mut bad_public = public;
    bad_public[0] ^= 1;
    assert!(!ed25519::verify(&bad_public, message, &signature));
}

#[test]
fn rfc8032_test_vectors() {
    // RFC 8032 section 7.1, tests 1-3.
    let vectors: [(&str, &str, &str, &str); 3] = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
             085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
             18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
    ];
    for (secret, public, message, signature) in vectors {
        let seed = array::<32>(secret);
        let public = array::<32>(public);
        let message = decode(message);
        let signature = array::<64>(signature);
        assert_eq!(ed25519::public_key_from_seed(&seed), public);
        assert!(ed25519::verify(&public, &message, &signature));
        assert_eq!(ed25519::sign(&seed, &message), signature);
    }
}

#[test]
fn cross_checked_signatures_from_an_independent_implementation() {
    // Signatures generated with Python `cryptography` (OpenSSL) for fixed
    // seeds and messages. Both directions matter: the verifier must accept
    // them, and our deterministic signer must reproduce them byte for byte.
    let vectors: [(&str, &str, &str, &str); 6] = [
        (
            "e24c2209a4ac6091c2f6ce50a5cd8522b9cabdbff69ba7d1c88f694237cf4a4f",
            "16f10011e0fa308caabb86b1c38e8e1bb029bc4f66fc834b9ed479d7c88091dd",
            "",
            "61f0a646982ddcc913d6fbcec57e7a63388a99932e56bc9ffeb3d600910a3fa7\
             6fcc503d551a12f3b94ca89ab9b98d31137fcfeb8638ac3626bba7f29395d608",
        ),
        (
            "6d6a88a81b56dbd577ab7350bffb2b77239b7fc57bf869ea40835a25bf864a22",
            "8a07c06aa65eacef41309bc2a5e09bb71790a2d2fb2635b6a7b77f4926e67076",
            "616263",
            "aa517546e997116a4d6ad79a06eb82a1f90752cf8382472b52cc5b8321067bb8\
             b35e31a7d7903cb55f48dbb8adcebab59940894344f822d24bb4f9adddcdca01",
        ),
        (
            "f5f9f78bbe5feee93fbf236e36144870ba5d9418f88eca87bfa6a96fa3940fd5",
            "a0666c0d19cb7048568b3578d1d262f10c73db37f8042ec6d3e477918f56fa39",
            "6165746865722063726f73732d636865636b",
            "40e5bdeb339c3062a3e78b106c5cd5797ab2f34d3fb0f4c7a3e317bcae151514\
             15b3a6f9f6e96da7e56c93c4fc7443907cec133b1db0c98c5c7848bb40078706",
        ),
        (
            "25da2596adccd642b152c9b08a547e20fe9466bb8c0002dcd39ffaea4c4d9951",
            "9e9dc55f3a6074672b2c4a680bc77c4648e58486f1bd9a0f204c8569934db7c4",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
            "c121ebba7b7edfaa5b303d2567ba236b5584f5f1dd41322efbcb40e75a8c3393\
             b7e038499154f2bf790753c065e13c8a60dab13fb4712961e7beddd4f203ff04",
        ),
        (
            "be3d667c37f0e9d8629a491f5304f7183ea4257cb192df50657f5b5dff20cfb6",
            "f831aa52324e5cd82cdf8b315d5abaa001d08a1fef7213e8d2dd3c1daf142eb7",
            "00070e151c232a31383f464d545b626970777e858c939aa1a8afb6bdc4cbd2d9\
             e0e7eef5fc030a11181f262d343b424950575e656c737a81888f969da4abb2b9\
             c0c7ced5dce3eaf1f8ff060d141b222930373e454c535a61686f767d848b9299\
             a0a7aeb5bcc3cad1d8dfe6edf4fb020910171e252c333a41484f565d646b7279\
             80",
            "a6ae444f689a7e705e56dbb29d24a7131aeb32f6db6a97f78d1bb974ed52617b\
             6707f0ec8a5f0240f25466acaaac6ba5c675866ac00119bf63c586701611130a",
        ),
        (
            "6f748f03d472fed62cb93d036fc499211e26062b3e25a065539cf356a4826ae9",
            "28d5640987712e2729372c099f346c5ceb8eaef7751cb547123872d0bf560854",
            "000d1a2734414e5b6875828f9ca9b6c3d0ddeaf704111e2b3845525f6c798693\
             a0adbac7d4e1eefb0815222f3c495663707d8a97a4b1becbd8e5f2ff0c192633\
             404d5a6774818e9ba8b5c2cfdce9f603101d2a3744515e6b7885929facb9c6d3\
             e0edfa0714212e3b4855626f7c8996a3b0bdcad7e4f1fe0b1825323f4c596673\
             808d9aa7b4c1cedbe8f5020f1c293643505d6a7784919eabb8c5d2dfecf90613\
             202d3a4754616e7b8895a2afbcc9d6e3f0fd0a1724313e4b5865727f8c99a6b3\
             c0cddae7f4010e1b2835424f5c697683909daab7c4d1deebf805121f2c394653\
             606d7a8794a1aebbc8d5e2effc091623303d4a5764717e8b98a5b2bfccd9e6f3\
             00",
            "5891ee3004d95c467ee59b9a0d3310cd7cde2688404ec5dec9219ecd6291fc9a\
             8de09868d3ecd6c579aea1dc063c59a45671b948107ff4c77020dd8c74397b0f",
        ),
    ];
    for (seed, public, message, signature) in vectors {
        let seed = array::<32>(seed);
        let public = array::<32>(public);
        let message = decode(message);
        let signature = array::<64>(signature);
        assert!(ed25519::verify(&public, &message, &signature));
        assert_eq!(ed25519::public_key_from_seed(&seed), public);
        assert_eq!(ed25519::sign(&seed, &message), signature);
    }
}

#[test]
fn small_order_public_keys_are_rejected() {
    // The identity point would otherwise satisfy the verification equation for
    // R = identity, S = 0, and any message.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let mut signature = [0u8; 64];
    signature[0] = 1;
    assert!(!ed25519::verify(&identity, b"any message", &signature));
}

#[test]
fn non_canonical_scalars_are_rejected() {
    let seed = array::<32>("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7");
    let public = ed25519::public_key_from_seed(&seed);
    let message = b"malleability";
    let signature = ed25519::sign(&seed, message);
    assert!(ed25519::verify(&public, message, &signature));

    // Replace S with a value >= L. The group equation would otherwise accept
    // S + L as a second encoding of the same signature.
    let mut tampered = signature;
    tampered[32..].copy_from_slice(&testing::ORDER_BYTES);
    assert!(!ed25519::verify(&public, message, &tampered));
}
