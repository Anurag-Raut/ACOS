import java.security.MessageDigest;
import java.security.Provider;
import java.security.SecureRandom;
import java.security.Security;
import java.util.Base64;

import javax.crypto.Cipher;
import javax.crypto.Mac;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * Deterministic java.security/javax.crypto fixture.
 *
 * Only deterministic primitives are printed: fixed-input digests, provider
 * names (which are stable for a given boot classpath), a fixed HMAC and a
 * fixed Base64 string. `SecureRandom` bytes are never printed.
 *
 * The second half (from `SHA-224=` on) is the ACOS extension. Its expected
 * values are the published FIPS 180-4 / FIPS 197 / NIST SP 800-38A vectors and
 * RFC 2202 (HMAC-SHA1), independently reproduced with Python's `hashlib`/
 * `hmac`; those lines are therefore marked `NIST-verified`, not captured from
 * a device.
 */
public class Main {
    private static final char[] HEX = "0123456789abcdef".toCharArray();

    static byte[] asciiBytes(String s) {
        byte[] b = new byte[s.length()];
        for (int i = 0; i < s.length(); i++) {
            b[i] = (byte) s.charAt(i);
        }
        return b;
    }

    static byte[] hx(String s) {
        byte[] b = new byte[s.length() / 2];
        for (int i = 0; i < b.length; i++) {
            b[i] = (byte) Integer.parseInt(s.substring(2 * i, 2 * i + 2), 16);
        }
        return b;
    }

    static String hex(byte[] b) {
        char[] c = new char[b.length * 2];
        for (int i = 0; i < b.length; i++) {
            int v = b[i] & 0xff;
            c[2 * i] = HEX[v >>> 4];
            c[2 * i + 1] = HEX[v & 0xf];
        }
        return new String(c);
    }

    static String digestHex(String algorithm, byte[] message) throws Exception {
        MessageDigest digest = MessageDigest.getInstance(algorithm);
        digest.update(message);
        return hex(digest.digest());
    }

    static void aes(String label, String mode, String keyHex, String ivHex, String ptHex)
            throws Exception {
        Cipher cipher = Cipher.getInstance(mode);
        SecretKeySpec key = new SecretKeySpec(hx(keyHex), "AES");
        if (ivHex == null) {
            cipher.init(Cipher.ENCRYPT_MODE, key);
        } else {
            cipher.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(hx(ivHex)));
        }
        System.out.println(label + "=" + hex(cipher.doFinal(hx(ptHex))));
    }

    public static void main(String[] args) throws Exception {
        byte[] message = asciiBytes("The quick brown fox jumps over the lazy dog");

        System.out.println("msg_len=" + message.length);
        for (String algorithm : new String[] {"MD5", "SHA-1", "SHA-256"}) {
            MessageDigest digest = MessageDigest.getInstance(algorithm);
            digest.update(message);
            System.out.println(algorithm + "=" + hex(digest.digest()));
        }

        SecureRandom random = SecureRandom.getInstance("SHA1PRNG");
        System.out.println("SecureRandom.algorithm=" + random.getAlgorithm());
        System.out.println("SecureRandom.provider=" + random.getProvider().getName());
        System.out.println("SecureRandom.seed_len=" + random.generateSeed(8).length);

        Provider[] providers = Security.getProviders();
        System.out.println("providers.count=" + providers.length);
        for (Provider provider : providers) {
            System.out.println("provider=" + provider.getName());
        }

        Mac mac = Mac.getInstance("HmacSHA256");
        mac.init(new SecretKeySpec(asciiBytes("acos-secret-key"), "HmacSHA256"));
        System.out.println("HmacSHA256=" + hex(mac.doFinal(message)));

        System.out.println("Base64=" + Base64.getEncoder()
                .encodeToString(asciiBytes("ACOS security digest")));

        // --- ACOS extension: NIST-verified known-answer vectors ---------------
        System.out.println("SHA-224=" + digestHex("SHA-224", message));
        System.out.println("SHA-384=" + digestHex("SHA-384", message));
        System.out.println("SHA-512=" + digestHex("SHA-512", message));

        Mac mac1 = Mac.getInstance("HmacSHA1");
        mac1.init(new SecretKeySpec(asciiBytes("acos-secret-key"), "HmacSHA1"));
        System.out.println("HmacSHA1=" + hex(mac1.doFinal(message)));

        // FIPS 197 / NIST SP 800-38A.
        aes("AES-128-ECB", "AES/ECB/NoPadding",
                "000102030405060708090a0b0c0d0e0f", null,
                "00112233445566778899aabbccddeeff");
        aes("AES-256-ECB", "AES/ECB/NoPadding",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", null,
                "00112233445566778899aabbccddeeff");
        aes("AES-128-CBC", "AES/CBC/NoPadding",
                "2b7e151628aed2a6abf7158809cf4f3c",
                "000102030405060708090a0b0c0d0e0f",
                "6bc1bee22e409f96e93d7e117393172a");
        aes("AES-128-CTR", "AES/CTR/NoPadding",
                "2b7e151628aed2a6abf7158809cf4f3c",
                "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
                "6bc1bee22e409f96e93d7e117393172a");

        System.out.println("done");
    }
}
