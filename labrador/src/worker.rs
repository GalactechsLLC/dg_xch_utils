use dg_xch_labrador::{
    MAGIC, MAX_BATCH, MAX_MESSAGE, MAX_PROOF, MAX_REQUEST, Profile, REQUEST, unpack_profile,
};
use std::{
    io::{Error, Read, Write},
    os::fd::FromRawFd,
};

unsafe extern "C" {
    fn dg_labrador_new(
        count: usize,
        h: *const i64,
        target: *const i64,
        context: *const u8,
        zk: i32,
    ) -> *mut libc::c_void;
    fn dg_labrador_set(job: *mut libc::c_void, slot: usize, s2: *const i64) -> i32;
    fn dg_labrador_prove(
        job: *mut libc::c_void,
        out: *mut u8,
        capacity: usize,
        length: *mut usize,
    ) -> i32;
    fn dg_labrador_verify(job: *mut libc::c_void, proof: *const u8, length: usize) -> i32;
    #[cfg(test)]
    fn dg_labrador_check(job: *mut libc::c_void) -> i32;
    fn dg_labrador_free(job: *mut libc::c_void);
}

struct Job(*mut libc::c_void);
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: An exclusively owned native handle, freed exactly once.
        unsafe {
            dg_labrador_free(self.0);
        }
    }
}

fn take<'a>(data: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
    if data.len() < length {
        return Err(Error::other("truncated input"));
    }
    let (value, rest) = data.split_at(length);
    *data = rest;
    Ok(value)
}
fn u16be(data: &mut &[u8]) -> Result<usize, Error> {
    Ok(usize::from(u16::from_be_bytes(
        take(data, 2)?.try_into().unwrap(),
    )))
}
fn public_key(bytes: &[u8]) -> Result<[i64; 512], Error> {
    if bytes.len() != 897 || bytes[0] != 9 {
        return Err(Error::other("wrong FN-DSA public key"));
    }
    let mut coefficients = [0u16; 512];
    fn_dsa_comm::codec::modq_decode(&bytes[1..], &mut coefficients)
        .ok_or_else(|| Error::other("noncanonical FN-DSA public key"))?;
    // FN-DSA 0.4 serializes h in NTT form; the native relation multiplies
    // coefficient polynomials modulo X^512+1. Undo that transform first.
    fn_dsa_comm::mq::mqpoly_ext_to_int(9, &mut coefficients);
    fn_dsa_comm::mq::mqpoly_NTT_to_int(9, &mut coefficients);
    fn_dsa_comm::mq::mqpoly_int_to_ext(9, &mut coefficients);
    Ok(coefficients.map(center))
}

fn center(value: u16) -> i64 {
    if value > 6144 {
        i64::from(value) - 12289
    } else {
        i64::from(value)
    }
}

fn signature(bytes: &[u8]) -> Result<[i64; 512], Error> {
    if bytes.len() != 666 || bytes[0] != 0x39 {
        return Err(Error::other("wrong FN-DSA signature"));
    }
    let mut coefficients = [0i16; 512];
    if !fn_dsa_comm::codec::comp_decode(&bytes[41..], &mut coefficients) {
        return Err(Error::other("noncanonical FN-DSA signature"));
    }
    Ok(coefficients.map(i64::from))
}

fn target(public_key: &[u8], salt: &[u8], message: &[u8]) -> [i64; 512] {
    let hashed_key = fn_dsa_comm::hashed_vrfykey_from_vrfykey(public_key);
    let mu = fn_dsa_comm::compute_mu(
        &hashed_key,
        &fn_dsa_comm::DomainContext(b"dg_xch/fn-dsa-512/v2"),
        &fn_dsa_comm::HASH_ID_RAW,
        message,
    );
    let mut coefficients = [0u16; 512];
    fn_dsa_comm::hash_to_point(salt, &mu, &mut coefficients);
    coefficients.map(center)
}

fn execute(mut request: &[u8]) -> Result<Vec<u8>, Error> {
    if take(&mut request, 8)? != REQUEST {
        return Err(Error::other("wrong worker protocol"));
    }
    let operation = take(&mut request, 1)?[0];
    if operation > 3 {
        return Err(Error::other("wrong operation"));
    }
    let profile = if operation >= 2 {
        Profile::Zk
    } else {
        Profile::Raw
    };
    let operation = operation % 2;
    let count = u16be(&mut request)?;
    if count == 0 || count > MAX_BATCH {
        return Err(Error::other("batch limit"));
    }
    let context = take(&mut request, 32)?;
    let mut keys = Vec::with_capacity(512 * count);
    let mut encoded_keys = Vec::with_capacity(count);
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        let encoded = take(&mut request, 897)?;
        keys.extend_from_slice(&public_key(encoded)?);
        encoded_keys.push(encoded);
        let length = u16be(&mut request)?;
        if length > MAX_MESSAGE {
            return Err(Error::other("message limit"));
        }
        messages.push(take(&mut request, length)?);
    }
    let mut witnesses = Vec::new();
    let mut salts = Vec::with_capacity(40 * count);
    let proof = if operation == 1 {
        for _ in 0..count {
            let raw = take(&mut request, 666)?;
            witnesses.push(signature(raw)?);
            salts.extend_from_slice(&raw[1..41]);
        }
        &[][..]
    } else {
        let length = u32::from_be_bytes(take(&mut request, 4)?.try_into().unwrap()) as usize;
        let envelope = take(&mut request, length)?;
        let (public_salts, body) = unpack_profile(profile, envelope, count)?;
        salts.extend_from_slice(public_salts);
        body
    };
    if !request.is_empty() {
        return Err(Error::other("trailing input"));
    }
    let mut targets = vec![0i64; 512 * count];
    for i in 0..count {
        targets[i * 512..(i + 1) * 512].copy_from_slice(&target(
            encoded_keys[i],
            &salts[i * 40..(i + 1) * 40],
            messages[i],
        ));
    }
    // SAFETY: Checked count fixes all array lengths; context is exactly 32 bytes.
    let job = Job(unsafe {
        dg_labrador_new(
            count,
            keys.as_ptr(),
            targets.as_ptr(),
            context.as_ptr(),
            i32::from(profile.zk()),
        )
    });
    if job.0.is_null() {
        return Err(Error::other("native statement failed"));
    }
    if operation == 0 {
        // SAFETY: Opaque live handle and bounded proof slice; no pointers retained.
        if unsafe { dg_labrador_verify(job.0, proof.as_ptr(), proof.len()) } != 1 {
            return Err(Error::other("invalid proof"));
        }
        return Ok(b"VERIFIED".to_vec());
    }
    for (i, witness) in witnesses.iter().enumerate() {
        // SAFETY: Every witness contains exactly 512 decoded coefficients.
        if unsafe { dg_labrador_set(job.0, i, witness.as_ptr()) } != 1 {
            return Err(Error::other("invalid Falcon relation"));
        }
    }
    let mut out = vec![0; MAX_PROOF];
    let prefix = 42 + salts.len();
    out[..8].copy_from_slice(MAGIC);
    out[8..40].copy_from_slice(profile.id());
    out[40..42].copy_from_slice(&(count as u16).to_be_bytes());
    out[42..prefix].copy_from_slice(&salts);
    let mut length = 0usize;
    // SAFETY: Bounded writable output and length, plus the live exclusive job.
    let outcome = unsafe {
        dg_labrador_prove(
            job.0,
            out[prefix..].as_mut_ptr(),
            MAX_PROOF - prefix,
            &mut length,
        )
    };
    if outcome != 1 || length > MAX_PROOF - prefix {
        return Err(Error::other(format!(
            "proof generation failed at native stage {outcome}"
        )));
    }
    out.truncate(prefix + length);
    Ok(out)
}

fn restrict() -> Result<(), Error> {
    // SAFETY: Limits apply only to this worker, before reading untrusted input.
    unsafe {
        for (resource, limit) in [
            (libc::RLIMIT_AS, 4 * 1024 * 1024 * 1024),
            (libc::RLIMIT_CPU, 165),
            (libc::RLIMIT_CORE, 0),
            (libc::RLIMIT_NOFILE, 16),
        ] {
            let value = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(resource, &value) != 0 {
                return Err(Error::last_os_error());
            }
        }
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
        {
            return Err(Error::last_os_error());
        }
        // Linux x86-64 only. After dynamic startup, the native arithmetic needs
        // memory, entropy and standard I/O, but no filesystem, network, exec or
        // process creation. Deny every other syscall with EPERM.
        let mut filter = vec![
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 4,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: 0xc000003e,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x80000000,
            },
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            },
        ];
        for call in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_close,
            libc::SYS_fstat,
            libc::SYS_brk,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mremap,
            libc::SYS_mprotect,
            libc::SYS_madvise,
            libc::SYS_getrandom,
            libc::SYS_futex,
            libc::SYS_clock_gettime,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_sigaltstack,
            libc::SYS_getpid,
            libc::SYS_gettid,
            libc::SYS_sched_yield,
            libc::SYS_exit,
            libc::SYS_exit_group,
        ] {
            filter.push(libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: call as u32,
            });
            filter.push(libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff0000,
            });
        }
        filter.push(libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | libc::EPERM as u32,
        });
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        if libc::prctl(libc::PR_SET_SECCOMP, 2, &program, 0, 0) != 0 {
            return Err(Error::last_os_error());
        }
    }
    Ok(())
}

fn main() {
    let result = (|| -> Result<(), Error> {
        if !std::is_x86_feature_detected!("avx2") {
            return Err(Error::other("worker requires AVX2"));
        }
        // Preserve a private protocol output descriptor; native diagnostics can
        // never be mistaken for proof bytes on stdout.
        // SAFETY: dup creates a new owned descriptor, then dup2 redirects fd 1.
        let mut output = unsafe {
            let fd = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
            if fd < 0 || libc::dup2(2, 1) < 0 {
                return Err(Error::last_os_error());
            }
            std::fs::File::from_raw_fd(fd)
        };
        restrict()?;
        let mut request = Vec::new();
        std::io::stdin()
            .take((MAX_REQUEST + 1) as u64)
            .read_to_end(&mut request)?;
        if request.len() > MAX_REQUEST {
            return Err(Error::other("request limit"));
        }
        output.write_all(&execute(&request)?)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_codecs() {
        let mut pk = [0u8; 897];
        pk[0] = 9;
        assert_eq!(public_key(&pk).unwrap(), [0; 512]);
        pk[1] = 255;
        pk[2] = 255;
        assert!(public_key(&pk).is_err());
        let mut sig = [0u8; 666];
        sig[0] = 0x39;
        // Each zero is sign=0, seven magnitude bits=0, unary terminator=1.
        for i in 0..512 {
            let p = 9 * i + 8;
            sig[41 + p / 8] |= 1 << (p % 8);
        }
        assert_eq!(signature(&sig).unwrap(), [0; 512]);
        sig[665] = 1;
        assert!(signature(&sig).is_err());
        sig[665] = 0;
        sig[41] |= 1;
        assert!(signature(&sig).is_err());
    }

    // Native code has one process-global commitment key, so native tests share
    // one test and create/drop each job before beginning the next statement.
    #[test]
    fn fn_dsa_native_relation_and_proof() {
        use fn_dsa::{KeyPairGenerator, SigningKey, VerifyingKey};
        struct TestRng(fn_dsa::SHAKE256);
        impl fn_dsa::CryptoRng for TestRng {}
        impl fn_dsa::RngCore for TestRng {
            fn next_u32(&mut self) -> u32 {
                let mut bytes = [0; 4];
                self.0.extract(&mut bytes);
                u32::from_le_bytes(bytes)
            }
            fn next_u64(&mut self) -> u64 {
                let mut bytes = [0; 8];
                self.0.extract(&mut bytes);
                u64::from_le_bytes(bytes)
            }
            fn fill_bytes(&mut self, bytes: &mut [u8]) {
                self.0.extract(bytes);
            }
            fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), fn_dsa::RngError> {
                self.fill_bytes(bytes);
                Ok(())
            }
        }
        let mut shake = fn_dsa::SHAKE256::new();
        shake.inject(b"public test-only FN-DSA LaBRADOR v2 fixture");
        shake.flip();
        let mut rng = TestRng(shake);
        let mut sk = [0; fn_dsa::sign_key_size(9)];
        let mut pk = [0; 897];
        fn_dsa::KeyPairGenerator512::default().keygen(9, &mut rng, &mut sk, &mut pk);
        let mut signer = fn_dsa::SigningKey512::decode(&sk).unwrap();
        let verifier = fn_dsa::VerifyingKey512::decode(&pk).unwrap();
        let message = b"native FN-DSA proof equivalence";
        let domain = fn_dsa::DomainContext(b"dg_xch/fn-dsa-512/v2");
        let mut sig = [0; 666];
        signer
            .sign(&mut rng, &domain, &fn_dsa::HASH_ID_RAW, message, &mut sig)
            .unwrap();
        assert!(verifier.verify(&sig, &domain, &fn_dsa::HASH_ID_RAW, message));
        let h = public_key(&pk).unwrap();
        let t = target(&pk, &sig[1..41], message);
        let s2 = signature(&sig).unwrap();
        let context = [42; 32];
        // SAFETY: All arrays have the fixed lengths required by the native API.
        for zk in [0, 1] {
            unsafe {
                let job = Job(dg_labrador_new(
                    1,
                    h.as_ptr(),
                    t.as_ptr(),
                    context.as_ptr(),
                    zk,
                ));
                assert!(!job.0.is_null());
                assert_eq!(dg_labrador_set(job.0, 0, s2.as_ptr()), 1);
                assert_eq!(dg_labrador_check(job.0), 1);
                let mut proof = vec![0; MAX_PROOF];
                let mut length = 0;
                assert_eq!(
                    dg_labrador_prove(job.0, proof.as_mut_ptr(), proof.len(), &mut length),
                    1
                );
                proof.truncate(length);
                assert_eq!(dg_labrador_verify(job.0, proof.as_ptr(), proof.len()), 1);
                drop(job);
                let wrong_target = target(&pk, &sig[1..41], b"different message");
                let wrong = Job(dg_labrador_new(
                    1,
                    h.as_ptr(),
                    wrong_target.as_ptr(),
                    context.as_ptr(),
                    zk,
                ));
                assert_eq!(dg_labrador_verify(wrong.0, proof.as_ptr(), proof.len()), 0);
            }
        }
        // A mixed batch must retain the explicit gadget for every slot,
        // even though this genuine signature's target does not need it.
        assert!(t.iter().any(|value| value.abs() > 840));
        let mut mixed_h = h.to_vec();
        mixed_h.extend_from_slice(&[0; 512]);
        let mut mixed_t = t.to_vec();
        mixed_t.extend_from_slice(&[0; 512]);
        mixed_t[512] = 840;
        let mut exceptional_s2 = [0; 512];
        exceptional_s2[0] = 840;
        unsafe {
            let job = Job(dg_labrador_new(
                2,
                mixed_h.as_ptr(),
                mixed_t.as_ptr(),
                context.as_ptr(),
                0,
            ));
            assert!(!job.0.is_null());
            assert_eq!(dg_labrador_set(job.0, 0, s2.as_ptr()), 1);
            assert_eq!(dg_labrador_set(job.0, 1, exceptional_s2.as_ptr()), 1);
            assert_eq!(dg_labrador_check(job.0), 1);
            assert_eq!(dg_labrador_set(job.0, 1, [0; 512].as_ptr()), 1);
            assert_eq!(dg_labrador_check(job.0), 0);
        }
        // Synthetic statements exercise the verifier's actual linear/BIN
        // constraints, bypassing signature decoding and early range guards.
        // h=0 means s1=target. All these vectors meet the old total norm bound.
        for (s1_value, s2_value, valid) in [
            (840, 840, true),
            (-840, -840, true),
            (841, 1, false),
            (-841, 1, false),
            (1, 841, false),
            (1, -841, false),
            (1, 0, false),
            (841, 0, false),
            (-841, 0, false),
        ] {
            let h = [0i64; 512];
            let mut t = [0i64; 512];
            t[0] = s1_value;
            let mut s2 = [0i64; 512];
            s2[0] = s2_value;
            unsafe {
                let job = Job(dg_labrador_new(
                    1,
                    h.as_ptr(),
                    t.as_ptr(),
                    context.as_ptr(),
                    0,
                ));
                assert_eq!(dg_labrador_set(job.0, 0, s2.as_ptr()), 1);
                assert_eq!(
                    dg_labrador_check(job.0) != 0,
                    valid,
                    "s1={s1_value}, s2={s2_value}"
                );
                if s1_value == 840 && s2_value == 840 {
                    // Exercise proof generation, not only relation checks,
                    // for the exceptional public-target layout as well.
                    let mut proof = vec![0; MAX_PROOF];
                    let mut length = 0;
                    assert_eq!(
                        dg_labrador_prove(job.0, proof.as_mut_ptr(), proof.len(), &mut length),
                        1
                    );
                    assert_eq!(dg_labrador_verify(job.0, proof.as_ptr(), length), 1);
                }
            }
        }
    }
}
