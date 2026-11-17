//! `stdlib/crypto.um`: SHA-256, HMAC, PBKDF2 and the encodings, checked against
//! the FIPS 180-4, RFC 4231 and RFC 7914 vectors (each re-derived with Python's
//! `hashlib`/`hmac`). Each test builds and runs a whole program.

use std::path::Path;
use std::process::Command;

fn toolchain_available() -> bool {
    // The host execution harness uses the Unix assembler/linker and executable
    // names. Windows coverage is the native smoke step in CI.
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("as") && ok("cc")
}

/// Shared helpers prepended to every program.
const HELPERS: &str = r##"
fn show_hex(p: *const u8, n: usize) {
    let src = Bytes::view(p, n)
    let buf = alloc(n * 2 + 1)
    let m = hex_encode(&src, buf)
    write_byte(buf.offset(m as isize), 0u8)
    println(buf as *const u8)
    dealloc(buf)
}
fn sha_of(b: &Bytes) {
    let d = alloc(32)
    sha256(b, d)
    show_hex(d as *const u8, 32)
    dealloc(d)
}
fn filled(n: usize, byte: u8) -> *mut u8 {
    let p = alloc(n + 1)
    let mut i: usize = 0
    while i < n {
        write_byte(p.offset(i as isize), byte)
        i = i + 1
    }
    return p
}
"##;

/// Builds `source`, runs it, and asserts what it printed.
fn expect_stdout(name: &str, source: &str, expected: &str) {
    if !toolchain_available() {
        return;
    }
    let dir = Path::new("/tmp/umay_crypto").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, format!("{HELPERS}{source}")).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay")).arg("build").arg(&file).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed for {name}:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist").join("main")).current_dir(&dir).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "stderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "exit status {:?}", out.status);
}

#[test]
fn sha256_vectors_and_padding_boundaries() {
    let source = r##"
fn main() -> i32 {
    let e = Bytes::from_str("")
    sha_of(&e)
    let abc = Bytes::from_str("abc")
    sha_of(&abc)
    let long = Bytes::from_str("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")
    sha_of(&long)
    let p64 = alloc(64)
    let mut i: usize = 0
    while i < 64 {
        write_byte(p64.offset(i as isize), i as u8)
        i = i + 1
    }
    let b64 = Bytes::view(p64 as *const u8, 64)
    sha_of(&b64)
    let p1000 = alloc(1000)
    i = 0
    while i < 1000 {
        write_byte(p1000.offset(i as isize), ((i * 7 + 3) % 256) as u8)
        i = i + 1
    }
    let b1000 = Bytes::view(p1000 as *const u8, 1000)
    sha_of(&b1000)
    let a = filled(120, 97u8)
    let lens = [55usize, 56usize, 63usize, 64usize, 65usize, 119usize, 120usize]
    i = 0
    while i < 7 {
        let v = Bytes::view(a as *const u8, lens[i])
        sha_of(&v)
        i = i + 1
    }
    return 0
}
"##;
    expect_stdout(
        "sha256",
        source,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
         ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n\
         248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1\n\
         fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108\n\
         1e9bc38cbf860b9ec31918b065f9b52476c549a782e0e7990bed8ce3868d2371\n\
         9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318\n\
         b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a\n\
         7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34\n\
         ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb\n\
         635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0\n\
         31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb\n\
         2f3d335432c70b580af0e8e1b3674a7c020d683aa5f73aaaedfdc55af904c21c\n",
    );
}

#[test]
fn hmac_sha256_rfc4231() {
    let source = r##"
fn mac(key: &Bytes, msg: &Bytes) {
    let d = alloc(32)
    hmac_sha256(key, msg, d)
    show_hex(d as *const u8, 32)
    dealloc(d)
}
fn main() -> i32 {
    let k1 = Bytes::view(filled(20, 11u8) as *const u8, 20)
    let m1 = Bytes::from_str("Hi There")
    mac(&k1, &m1)
    let k2 = Bytes::from_str("Jefe")
    let m2 = Bytes::from_str("what do ya want for nothing?")
    mac(&k2, &m2)
    let k6 = Bytes::view(filled(131, 170u8) as *const u8, 131)
    let m6 = Bytes::from_str("Test Using Larger Than Block-Size Key - Hash Key First")
    mac(&k6, &m6)
    return 0
}
"##;
    expect_stdout(
        "hmac",
        source,
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7\n\
         5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843\n\
         60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54\n",
    );
}

#[test]
fn pbkdf2_sha256_rfc7914() {
    let source = r##"
fn derive(pw: &str, salt: &str, iterations: u32, n: usize) {
    let p = Bytes::from_str(pw)
    let s = Bytes::from_str(salt)
    let out = alloc(n)
    pbkdf2_sha256(&p, &s, iterations, out, n)
    show_hex(out as *const u8, n)
    dealloc(out)
}
fn main() -> i32 {
    derive("password", "salt", 1u32, 32)
    derive("password", "salt", 2u32, 32)
    derive("password", "salt", 4096u32, 32)
    derive("passwordPASSWORDpassword", "saltSALTsaltSALTsaltSALTsaltSALTsalt", 4096u32, 40)
    return 0
}
"##;
    expect_stdout(
        "pbkdf2",
        source,
        "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b\n\
         ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43\n\
         c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a\n\
         348c89dbcbd32b2f32d814b8116e84cf2b17347ebc1800181c4e2a1fb8dd53e1c635518c7dac47e9\n",
    );
}

#[test]
fn base64url_round_trips_and_rejects() {
    let source = r##"
fn round_trip(text: &str, n: usize) {
    let src = Bytes::view(text as *const u8, n)
    let enc = alloc(16)
    let m = base64url_encode(&src, enc)
    write_byte(enc.offset(m as isize), 0u8)
    print("[")
    print(enc as *const u8)
    print("] ")
    let e = Bytes::view(enc as *const u8, m)
    let dec = alloc(16)
    let k = base64url_decode(&e, dec)
    let back = Bytes::view(dec as *const u8, k as usize)
    if k == n as i64 && constant_time_equals(&src, &back) {
        println("ok")
    } else {
        println("MISMATCH")
    }
}
fn bad(text: &str) {
    let src = Bytes::from_str(text)
    let out = alloc(16)
    println_i64(base64url_decode(&src, out))
}
fn main() -> i32 {
    round_trip("fooba", 0)
    round_trip("fooba", 1)
    round_trip("fooba", 2)
    round_trip("fooba", 3)
    round_trip("fooba", 4)
    round_trip("fooba", 5)
    let raw = alloc(3)
    write_byte(raw, 251u8)
    write_byte(raw.offset(1), 255u8)
    write_byte(raw.offset(2), 254u8)
    let r = Bytes::view(raw as *const u8, 3)
    let enc = alloc(8)
    let m = base64url_encode(&r, enc)
    write_byte(enc.offset(m as isize), 0u8)
    println(enc as *const u8)
    bad("Zm9v=")
    bad("Zm9vY")
    bad("Zm+v")
    bad("Zm/v")
    bad("Zh")
    bad("Zm9")
    return 0
}
"##;
    expect_stdout(
        "base64url",
        source,
        "[] ok\n[Zg] ok\n[Zm8] ok\n[Zm9v] ok\n[Zm9vYg] ok\n[Zm9vYmE] ok\n-__-\n-1\n-1\n-1\n-1\n-1\n-1\n",
    );
}

#[test]
fn constant_time_equals_and_hex() {
    let source = r##"
fn eq(a: &str, b: &str) {
    let x = Bytes::from_str(a)
    let y = Bytes::from_str(b)
    if constant_time_equals(&x, &y) { println("true") } else { println("false") }
}
fn main() -> i32 {
    eq("signature", "signature")
    eq("signature", "signaturf")
    eq("signature", "Signature")
    eq("sig", "signature")
    eq("", "")
    let h = Bytes::from_str("\nAaz")
    show_hex(h.data, h.len)
    return 0
}
"##;
    expect_stdout("ct_equals", source, "true\nfalse\nfalse\nfalse\ntrue\n0a41617a\n");
}
