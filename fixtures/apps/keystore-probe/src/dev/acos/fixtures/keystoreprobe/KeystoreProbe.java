package dev.acos.fixtures.keystoreprobe;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyInfo;
import android.security.keystore.KeyNotYetValidException;
import android.security.keystore.KeyExpiredException;
import android.security.keystore.KeyProperties;
import android.util.Base64;

import java.io.File;
import java.io.FileOutputStream;
import java.io.FileInputStream;
import java.io.InputStream;
import java.io.StringWriter;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.attribute.PosixFilePermission;
import java.security.InvalidKeyException;
import java.security.Key;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.KeyStoreException;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.cert.Certificate;
import java.security.cert.X509Certificate;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Date;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Properties;
import java.util.Set;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.Mac;
import javax.crypto.SecretKey;
import javax.crypto.SecretKeyFactory;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * K5b probe: AndroidKeyStore fidelity and cross-session reach, measured inside
 * a real ART session (this Activity runs under LaunchMain, exactly like the
 * rest of the corpus).
 *
 * <p>Mode comes from {@code ACOS_ART_KEYPROBE_MODE}:
 * <ul>
 *   <li>{@code fidelity} — the AOSP-behaviour assertions (KeyInfo, purposes,
 *       validity, import, certificate chain, attestation refusal, file modes);</li>
 *   <li>{@code create} — session A: mint a key and a private file, then report
 *       the paths (the driver holds the process with
 *       {@code ACOSLAUNCH_HOLD_MS});</li>
 *   <li>{@code probe} — session B: try to reach A's alias and files by path;</li>
 *   <li>{@code default_off} — with the shim not requested, the honest
 *       {@code AndroidKeyStore not found} failure must still be the result.</li>
 * </ul>
 *
 * <p>Every assertion is written as one JSON line to
 * {@code <files>/keystore-probe/<mode>.jsonl} (host-readable inside the run
 * root) and echoed to stdout with a {@code KEYPROBE } prefix. A check that
 * cannot run is a failure, never a skip.
 */
public final class KeystoreProbe {
    private static final String TAG = "KEYPROBE";
    private final List<String> lines = new ArrayList<>();
    /** Paths to reach for in `probe` mode (arguments after the package). */
    private final List<String> extraTargets = new ArrayList<>();
    private final java.io.File dataDir;
    private final String pkg;
    private int pass;
    private int fail;

    KeystoreProbe(java.io.File dataDir, String pkg) {
        this.dataDir = dataDir;
        this.pkg = pkg;
    }

    /**
     * Run one mode in a plain ART guest (`acos-art run`), printing every check
     * to stdout. Usage: {@code <mode> <data-dir> <package>}.
     */
    public static void main(String[] args) {
        String mode = args.length > 0 ? args[0] : "fidelity";
        java.io.File dataDir = new java.io.File(args.length > 1 ? args[1]
                : "/data/app-data/dev.acos.fixtures.keystoreprobe/files");
        String pkg = args.length > 2 ? args[2] : "dev.acos.fixtures.keystoreprobe";
        KeystoreProbe probe = new KeystoreProbe(dataDir, pkg);
        for (int i = 3; i < args.length; i++) {
            if (args[i] != null && !args[i].isEmpty()) probe.extraTargets.add(args[i]);
        }
        probe.run(mode);
    }

    /** Entry point shared with the APK activity (which passes its files dir). */
    void run(String mode) {
        System.out.println(TAG + " start mode=" + mode);
        System.out.flush();
        // The provider install call is the same one LaunchMain.main makes
        // before app code, with the same session-private directory.
        if (!"default_off".equals(mode)) {
            mkdirs(dataDir);
            dev.acos.art.SoftwareKeyStore.install(dataDir.getParentFile().getPath());
        }
        try {
            if ("create".equals(mode)) {
                create();
            } else if ("probe".equals(mode)) {
                probe();
            } else if ("default_off".equals(mode)) {
                defaultOff();
            } else if ("retention".equals(mode)) {
                retention();
            } else {
                fidelity();
            }
        } catch (Throwable t) {
            t.printStackTrace(System.out);
            check(mode + ".harness", false, "harness threw " + t);
        }
        try {
            finish(mode);
        } catch (Throwable t) {
            t.printStackTrace(System.out);
            throw new RuntimeException(t);
        }
    }

    // ------------------------------------------------------------------ modes

    /** Session A: a key plus an app-private file, both inside this run's data. */
    private void create() throws Exception {
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        KeyGenerator kg = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        kg.init(new KeyGenParameterSpec.Builder("k_a",
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build());
        SecretKey key = kg.generateKey();
        check("create.key", key != null && ks.containsAlias("k_a"),
                "alias=k_a size=" + key.getEncoded().length * 8);

        File files = dataDir;
        File dir = new File(files, "keystore-probe");
        mkdirs(dir);
        File priv = new File(dir, "private-a.txt");
        writePrivate(priv, "A-SECRET-" + Base64.encodeToString(key.getEncoded(), Base64.NO_WRAP));
        check("create.private_file", priv.isFile() && priv.length() > 0, priv.getPath());
        // The paths a peer session could plausibly name.
        check("create.paths",
                true,
                priv.getPath() + "|" + new File(new File(dataDir.getParentFile(),
                        "acos-keystore"), "entries.properties").getPath());
    }

    /** Session B: every reach for A's material must fail; controls must work. */
    private void probe() throws Exception {
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);

        // Control 1: B can read its own key by alias. Without this, a null for
        // A's alias would prove nothing.
        KeyGenerator kg = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        kg.init(new KeyGenParameterSpec.Builder("k_b",
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build());
        kg.generateKey();
        check("control.own_alias_readable", ks.getKey("k_b", null) != null,
                "k_b is B's own key");

        // Control 2: B can read a file it wrote by path. Without this, a failed
        // read of A's path would prove nothing.
        File mine = new File(new File(dataDir, "keystore-probe"), "private-b.txt");
        mkdirs(mine.getParentFile());
        writePrivate(mine, "B-OWN");
        check("control.own_file_readable", "B-OWN".equals(read(mine)),
                mine.getPath());

        // Attempt 1: A's key alias through the keystore API.
        boolean aliasVisible = ks.containsAlias("k_a") || ks.getKey("k_a", null) != null;
        check("xsession.alias_absent", !aliasVisible,
                aliasVisible ? "LEAK: k_a readable from session B" : "k_a absent");

        // Attempt 2/3: A's key-material file and private file, by every path a
        // peer session can name: A's in-sandbox path and A's host path.
        Set<String> targets = new LinkedHashSet<>(extraTargets);
        for (String t : env("ACOS_ART_KEYPROBE_TARGETS", "").split(",")) {
            if (!t.trim().isEmpty()) targets.add(t.trim());
        }
        targets.add(new File(new File(dataDir, "keystore-probe"), "private-a.txt").getPath());
        targets.add(new File(new File(dataDir.getParentFile(), "acos-keystore"),
                "entries.properties").getPath());
        for (String t : targets) {
            File f = new File(t);
            boolean leaked = false;
            String why;
            try {
                String content = read(f);
                leaked = content != null && (content.contains("A-SECRET")
                        || content.contains("k_a"));
                why = leaked ? "LEAK: read " + content.length() + " bytes containing A's material"
                        : "readable but without A's material";
            } catch (Throwable e) {
                why = "refused: " + e.getClass().getSimpleName()
                        + (e.getMessage() == null ? "" : ": " + e.getMessage());
            }
            String name = "xsession.path_absent[" + t + "]";
            check(name, !leaked, why + (f.exists() ? " (exists)" : " (absent)"));
        }

        // Attempt 4: A's host-side run root is not even mounted in this
        // session's namespace, which is *why* the by-path attempts fail.
        check("xsession.host_root_absent",
                !new File("/home").exists() && !new File("/proc/1/root/home").exists(),
                "root=" + Arrays.toString(new File("/").list()));
    }

    /** Item 3: with no ACOS_ART_KEYSTORE the honest failure must survive. */
    private void defaultOff() {
        String outcome;
        boolean honest;
        try {
            KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
            ks.load(null);
            outcome = "provider present (unexpected: the shim is opt-in)";
            honest = false;
        } catch (KeyStoreException e) {
            outcome = e.toString();
            honest = outcome.contains("AndroidKeyStore not found");
        } catch (Throwable e) {
            outcome = e.toString();
            honest = false;
        }
        check("default_off.not_found", honest, outcome);
        check("default_off.provider_absent",
                java.security.Security.getProvider("AndroidKeyStore") == null,
                "providers=" + providerNames());
    }

    /**
     * Retention: a *second* app process in the same session's app-data.
     *
     * <p>ACOS relaunches an app with an empty app-data directory on purpose
     * (`crates/acos-cli/src/engine/serve.rs`: "the same APK relaunched with
     * empty app-data"), so the device's "keys survive a process restart" is a
     * documented fidelity gap. What this mode asserts is the pair of facts that
     * *are* true here: the store is on disk and a fresh process in the same
     * directory reloads it, while a fresh session does not see it.
     */
    private void retention() throws Exception {
        File store = new File(new File(dataDir.getParentFile(), "acos-keystore"),
                "entries.properties");
        check("retention.store_on_disk", store.isFile(),
                store.getPath() + " exists=" + store.exists());
        Properties p = new Properties();
        try (InputStream in = new FileInputStream(store)) {
            p.load(in);
        }
        check("retention.alias_persisted", p.getProperty("k_a.type") != null,
                "entries=" + p.stringPropertyNames().size());
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        check("retention.alias_readable", ks.getKey("k_a", null) != null,
                "k_a readable in this run");
    }

    // -------------------------------------------------------------- fidelity

    private void fidelity() throws Exception {
        check("fidelity.provider", java.security.Security.getProvider("AndroidKeyStore") != null,
                "providers=" + providerNames());
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        check("fidelity.store_empty_at_start", ks.size() == 0, "size=" + ks.size());

        // --- AES + KeyInfo -----------------------------------------------
        KeyGenerator kg = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        kg.init(new KeyGenParameterSpec.Builder("k_aes",
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM, KeyProperties.BLOCK_MODE_CBC)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE,
                        KeyProperties.ENCRYPTION_PADDING_PKCS7)
                .build());
        SecretKey aes = kg.generateKey();
        SecretKeyFactory skf = SecretKeyFactory.getInstance("AES", "AndroidKeyStore");
        KeyInfo info = (KeyInfo) skf.getKeySpec(aes, KeyInfo.class);
        check("fidelity.keyinfo.size", info.getKeySize() == 256, "size=" + info.getKeySize());
        check("fidelity.keyinfo.purposes",
                info.getPurposes() == (KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT),
                "purposes=0x" + Integer.toHexString(info.getPurposes()));
        check("fidelity.keyinfo.software",
                !info.isInsideSecureHardware()
                        && info.getSecurityLevel() == KeyProperties.SECURITY_LEVEL_SOFTWARE,
                "insideSecureHardware=" + info.isInsideSecureHardware()
                        + " securityLevel=" + info.getSecurityLevel());
        check("fidelity.keyinfo.origin",
                info.getOrigin() == KeyProperties.ORIGIN_GENERATED,
                "origin=" + info.getOrigin());
        check("fidelity.keyinfo.alias", "k_aes".equals(info.getKeystoreAlias()),
                "alias=" + info.getKeystoreAlias());
        check("fidelity.keyinfo.digests_modes",
                Arrays.asList(info.getBlockModes()).contains(KeyProperties.BLOCK_MODE_GCM)
                        && Arrays.asList(info.getBlockModes()).contains(KeyProperties.BLOCK_MODE_CBC),
                "blockModes=" + Arrays.toString(info.getBlockModes()));
        check("fidelity.creation_date", ks.getCreationDate("k_aes") != null,
                String.valueOf(ks.getCreationDate("k_aes")));

        // AES roundtrip must still work (the guard is not over-blocking).
        Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
        c.init(Cipher.ENCRYPT_MODE, aes);
        byte[] iv = c.getIV();
        byte[] ct = c.doFinal("acos".getBytes("UTF-8"));
        Cipher d = Cipher.getInstance("AES/GCM/NoPadding");
        d.init(Cipher.DECRYPT_MODE, aes, new javax.crypto.spec.GCMParameterSpec(128, iv));
        check("fidelity.cipher.roundtrip", "acos".equals(new String(d.doFinal(ct), "UTF-8")),
                "ct=" + ct.length + " iv=" + iv.length);

        // --- purposes are enforced ---------------------------------------
        // AOSP validates purposes against the algorithm at generation
        // (KeyGenParameterSpec / keystore2 KeyCharacteristics): an AES key
        // cannot sign, so that request must be refused rather than granted.
        boolean impossibleRefused;
        String impossibleWhy;
        try {
            KeyGenerator bad = KeyGenerator.getInstance("AES", "AndroidKeyStore");
            bad.init(new KeyGenParameterSpec.Builder("k_bad_purpose", KeyProperties.PURPOSE_SIGN)
                    .setKeySize(256)
                    .setBlockModes(KeyProperties.BLOCK_MODE_CBC)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .build());
            impossibleRefused = false;
            impossibleWhy = "accepted SIGN for an AES key";
        } catch (java.security.InvalidAlgorithmParameterException e) {
            impossibleRefused = true;
            impossibleWhy = e.getMessage();
        }
        check("fidelity.purposes.impossible_rejected", impossibleRefused, impossibleWhy);

        // An encrypt-only AES key must encrypt and must refuse decrypt
        // (keystore2 ErrorCode::INCOMPATIBLE_PURPOSE -> InvalidKeyException).
        KeyGenerator eo = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        eo.init(new KeyGenParameterSpec.Builder("k_encrypt_only", KeyProperties.PURPOSE_ENCRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_CBC)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_PKCS7)
                .build());
        SecretKey encryptOnly = eo.generateKey();
        boolean encryptOk;
        String encryptWhy;
        try {
            Cipher enc = Cipher.getInstance("AES/CBC/PKCS7Padding");
            enc.init(Cipher.ENCRYPT_MODE, encryptOnly);
            encryptOk = enc.doFinal("acos".getBytes("UTF-8")).length > 0;
            encryptWhy = "encrypt ok iv=" + enc.getIV().length;
        } catch (Throwable t) {
            encryptOk = false;
            encryptWhy = "encrypt refused: " + t;
        }
        check("fidelity.purpose.encrypt_only_allows_encrypt", encryptOk, encryptWhy);
        boolean decryptRefused;
        String decryptWhy;
        try {
            Cipher dec = Cipher.getInstance("AES/CBC/PKCS7Padding");
            dec.init(Cipher.DECRYPT_MODE, encryptOnly, new IvParameterSpec(new byte[16]));
            decryptRefused = false;
            decryptWhy = "DECRYPT succeeded for an encrypt-only key";
        } catch (Throwable e) {
            String message = String.valueOf(e.getMessage());
            decryptRefused = message.startsWith("INCOMPATIBLE_PURPOSE:")
                    && message.contains("PURPOSE_DECRYPT");
            decryptWhy = message;
        }
        check("fidelity.purpose.encrypt_only_refuses_decrypt", decryptRefused, decryptWhy);

        // Randomized-encryption-required: a caller IV on ENCRYPT is refused.
        boolean ivRefused;
        String ivWhy;
        try {
            Cipher gcm = Cipher.getInstance("AES/GCM/NoPadding");
            gcm.init(Cipher.ENCRYPT_MODE, aes, new javax.crypto.spec.GCMParameterSpec(
                    128, new byte[12]));
            ivRefused = false;
            ivWhy = "caller IV accepted on a randomised-encryption key";
        } catch (Throwable e) {
            String message = String.valueOf(e.getMessage());
            ivRefused = message.startsWith("INCOMPATIBLE_RANDOMIZED_ENCRYPTION_REQUIRED:");
            ivWhy = message;
        }
        check("fidelity.randomized_iv_refused", ivRefused, ivWhy);

        // An RSA sign-only key must sign and must refuse encryption.
        KeyPairGenerator signRsa = KeyPairGenerator.getInstance("RSA", "AndroidKeyStore");
        signRsa.initialize(new KeyGenParameterSpec.Builder("k_rsa_sign",
                KeyProperties.PURPOSE_SIGN)
                .setKeySize(2048)
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setSignaturePaddings(KeyProperties.SIGNATURE_PADDING_RSA_PKCS1)
                .build());
        KeyPair signPair = signRsa.generateKeyPair();
        java.security.Signature signer = java.security.Signature.getInstance("SHA256withRSA");
        signer.initSign(signPair.getPrivate());
        signer.update("payload".getBytes("UTF-8"));
        boolean signOk = signer.sign().length > 0;
        check("fidelity.purpose.rsa_sign_only_allows_sign", signOk,
                "signature bytes=" + (signOk ? ">0" : "0"));
        boolean rsaEncryptRefused;
        String rsaWhy;
        try {
            Cipher rsaEnc = Cipher.getInstance("RSA/ECB/PKCS1Padding");
            rsaEnc.init(Cipher.ENCRYPT_MODE, signPair.getPrivate());
            rsaEncryptRefused = false;
            rsaWhy = "ENCRYPT succeeded for a sign-only RSA key";
        } catch (Throwable e) {
            String message = String.valueOf(e.getMessage());
            rsaEncryptRefused = message.startsWith("INCOMPATIBLE_PURPOSE:")
                    && message.contains("PURPOSE_ENCRYPT");
            rsaWhy = message;
        }
        check("fidelity.purpose.rsa_sign_only_refuses_encrypt", rsaEncryptRefused, rsaWhy);

        // ... and signing purposes are honest about what they allow.
        KeyGenerator hg = KeyGenerator.getInstance("HmacSHA256", "AndroidKeyStore");
        hg.init(new KeyGenParameterSpec.Builder("k_hmac", KeyProperties.PURPOSE_SIGN)
                .setKeySize(256).setDigests(KeyProperties.DIGEST_SHA256).build());
        SecretKey hmac = hg.generateKey();
        Mac mac = Mac.getInstance("HmacSHA256");
        mac.init(hmac);
        check("fidelity.purpose.sign_allowed",
                mac.doFinal("payload".getBytes("UTF-8")).length == 32, "");

        // --- validity window ---------------------------------------------
        long now = System.currentTimeMillis();
        KeyGenerator future = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        future.init(new KeyGenParameterSpec.Builder("k_not_yet",
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeyValidityStart(new Date(now + 3600_000L))
                .build());
        SecretKey notYet = future.generateKey();
        try {
            Cipher.getInstance("AES/GCM/NoPadding").init(Cipher.ENCRYPT_MODE, notYet);
            check("fidelity.validity.not_yet_valid", false, "used a not-yet-valid key");
        } catch (Throwable e) {
            String message = String.valueOf(e.getMessage());
            check("fidelity.validity.not_yet_valid", message.startsWith("KEY_NOT_YET_VALID:"),
                    message);
        }

        KeyGenerator expired = KeyGenerator.getInstance("AES", "AndroidKeyStore");
        expired.init(new KeyGenParameterSpec.Builder("k_expired",
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeyValidityEnd(new Date(now - 3600_000L))
                .build());
        SecretKey dead = expired.generateKey();
        try {
            Cipher.getInstance("AES/GCM/NoPadding").init(Cipher.ENCRYPT_MODE, dead);
            check("fidelity.validity.expired", false, "used an expired key");
        } catch (Throwable e) {
            String message = String.valueOf(e.getMessage());
            check("fidelity.validity.expired", message.startsWith("KEY_EXPIRED:"), message);
        }

        // --- import honours ProtectionParameter --------------------------
        SecretKeySpec raw = new SecretKeySpec(new byte[32], "AES");
        try {
            ks.setEntry("k_import_bad", new KeyStore.SecretKeyEntry(raw),
                    new KeyStore.ProtectionParameter() {
                    });
            check("fidelity.import.protection_parameter", false, "accepted a foreign ProtParam");
        } catch (KeyStoreException e) {
            check("fidelity.import.protection_parameter",
                    e.getMessage() != null && e.getMessage().contains("Unsupported protection"),
                    e.getMessage());
        }
        ks.setEntry("k_import", new KeyStore.SecretKeyEntry(raw), null);
        KeyInfo imported = (KeyInfo) SecretKeyFactory.getInstance("AES", "AndroidKeyStore")
                .getKeySpec((SecretKey) ks.getKey("k_import", null), KeyInfo.class);
        check("fidelity.import.secret",
                imported.getOrigin() == KeyProperties.ORIGIN_IMPORTED
                        && imported.getKeySize() == 256
                        && Arrays.equals(ks.getKey("k_import", null).getEncoded(), raw.getEncoded()),
                "origin=" + imported.getOrigin() + " size=" + imported.getKeySize());

        // --- generated asymmetric keys get a chain ------------------------
        KeyPairGenerator rsa = KeyPairGenerator.getInstance("RSA", "AndroidKeyStore");
        rsa.initialize(new KeyGenParameterSpec.Builder("k_rsa",
                KeyProperties.PURPOSE_SIGN | KeyProperties.PURPOSE_VERIFY
                        | KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(2048)
                .setDigests(KeyProperties.DIGEST_SHA256, KeyProperties.DIGEST_SHA1)
                .setSignaturePaddings(KeyProperties.SIGNATURE_PADDING_RSA_PKCS1)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_RSA_PKCS1)
                .build());
        KeyPair rsaPair = rsa.generateKeyPair();
        Certificate[] chain = ks.getCertificateChain("k_rsa");
        boolean chainOk = chain != null && chain.length == 1
                && chain[0] instanceof X509Certificate;
        String chainDetail = chain == null ? "null chain" : "len=" + chain.length;
        if (chainOk) {
            X509Certificate cert = (X509Certificate) chain[0];
            chainDetail += " subject=" + cert.getSubjectDN().getName()
                    + " issuer=" + cert.getIssuerDN().getName();
            try {
                cert.verify(cert.getPublicKey());
                chainOk = cert.getSubjectDN().equals(cert.getIssuerDN())
                        && cert.getPublicKey().getEncoded()
                                .length == rsaPair.getPublic().getEncoded().length;
                chainDetail += " selfSigned=true";
            } catch (Throwable t) {
                chainOk = false;
                chainDetail += " verify-failed=" + t;
            }
            check("fidelity.chain.verifies", chainOk, chainDetail);
        } else {
            check("fidelity.chain.verifies", false, chainDetail);
        }
        KeyInfo rsaInfo = (KeyInfo) KeyFactory.getInstance("RSA", "AndroidKeyStore")
                .getKeySpec(rsaPair.getPrivate(), KeyInfo.class);
        check("fidelity.keyinfo.rsa",
                rsaInfo.getKeySize() == 2048
                        && (rsaInfo.getPurposes() & KeyProperties.PURPOSE_SIGN) != 0
                        && !rsaInfo.isInsideSecureHardware(),
                "size=" + rsaInfo.getKeySize() + " purposes=0x"
                        + Integer.toHexString(rsaInfo.getPurposes()));

        KeyPairGenerator ec = KeyPairGenerator.getInstance("EC", "AndroidKeyStore");
        ec.initialize(new KeyGenParameterSpec.Builder("k_ec",
                KeyProperties.PURPOSE_SIGN | KeyProperties.PURPOSE_VERIFY)
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setAlgorithmParameterSpec(
                        new java.security.spec.ECGenParameterSpec("secp256r1"))
                .build());
        KeyPair ecPair = ec.generateKeyPair();
        Certificate[] ecChain = ks.getCertificateChain("k_ec");
        boolean ecOk = ecChain != null && ecChain.length == 1;
        if (ecOk) {
            try {
                ecChain[0].verify(ecChain[0].getPublicKey());
                ecOk = ((X509Certificate) ecChain[0]).getSubjectDN()
                        .equals(((X509Certificate) ecChain[0]).getIssuerDN());
            } catch (Throwable t) {
                ecOk = false;
            }
        }
        KeyInfo ecInfo = (KeyInfo) KeyFactory.getInstance("EC", "AndroidKeyStore")
                .getKeySpec(ecPair.getPrivate(), KeyInfo.class);
        check("fidelity.chain.ec", ecOk && ecInfo.getKeySize() == 256,
                "chain=" + (ecChain == null ? "null" : ecChain.length)
                        + " size=" + ecInfo.getKeySize());

        // --- attestation is refused, explicitly ---------------------------
        try {
            KeyGenerator att = KeyGenerator.getInstance("AES", "AndroidKeyStore");
            att.init(new KeyGenParameterSpec.Builder("k_att",
                    KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                    .setKeySize(256)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setAttestationChallenge("acos-k5b".getBytes("UTF-8"))
                    .build());
            att.generateKey();
            check("fidelity.attestation.refused", false, "attestation silently accepted");
        } catch (java.security.InvalidAlgorithmParameterException e) {
            check("fidelity.attestation.refused",
                    e.getMessage() != null
                            && e.getMessage().startsWith("UNSUPPORTED_ATTESTATION:"),
                    e.getMessage());
        }

        // --- unsupported characteristics fail, they do not degrade --------
        try {
            KeyGenerator ua = KeyGenerator.getInstance("AES", "AndroidKeyStore");
            ua.init(new KeyGenParameterSpec.Builder("k_ua",
                    KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                    .setKeySize(256)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setUserAuthenticationRequired(true)
                    .build());
            ua.generateKey();
            check("fidelity.userauth.refused", false, "accepted user-auth requirement");
        } catch (java.security.InvalidAlgorithmParameterException e) {
            check("fidelity.userauth.refused", true, e.getMessage());
        }

        // --- on-disk material is owner-only -------------------------------
        File storeDir = new File(dataDir.getParentFile(), "acos-keystore");
        File storeFile = new File(storeDir, "entries.properties");
        Set<PosixFilePermission> filePerms = posix(storeFile);
        Set<PosixFilePermission> dirPerms = posix(storeDir);
        check("fidelity.persist.mode_0600",
                filePerms.equals(new LinkedHashSet<>(Arrays.asList(
                        PosixFilePermission.OWNER_READ, PosixFilePermission.OWNER_WRITE)))
                        && dirPerms.equals(new LinkedHashSet<>(Arrays.asList(
                        PosixFilePermission.OWNER_READ, PosixFilePermission.OWNER_WRITE,
                        PosixFilePermission.OWNER_EXECUTE))),
                "file=" + filePerms + " dir=" + dirPerms);
        check("fidelity.persist.no_group_world", !anyGroupOrOther(filePerms)
                        && !anyGroupOrOther(dirPerms),
                "file=" + filePerms);

        // --- absence is structured, not a crash --------------------------
        check("fidelity.absent_alias", ks.getKey("no-such-alias", null) == null,
                "getKey returned null");
        boolean unknownAlg;
        try {
            KeyGenerator.getInstance("DESede", "AndroidKeyStore");
            unknownAlg = false;
        } catch (java.security.NoSuchAlgorithmException e) {
            unknownAlg = true;
        }
        check("fidelity.unsupported_algorithm", unknownAlg,
                "DESede refused with NoSuchAlgorithmException");
    }

    // --------------------------------------------------------------- helpers

    private static boolean anyGroupOrOther(Set<PosixFilePermission> perms) {
        return perms.contains(PosixFilePermission.GROUP_READ)
                || perms.contains(PosixFilePermission.GROUP_WRITE)
                || perms.contains(PosixFilePermission.GROUP_EXECUTE)
                || perms.contains(PosixFilePermission.OTHERS_READ)
                || perms.contains(PosixFilePermission.OTHERS_WRITE)
                || perms.contains(PosixFilePermission.OTHERS_EXECUTE);
    }

    private static Set<PosixFilePermission> posix(File f) {
        try {
            Path p = Paths.get(f.getPath());
            return Files.getPosixFilePermissions(p);
        } catch (Throwable t) {
            return Collections0.empty();
        }
    }

    private static final class Collections0 {
        static Set<PosixFilePermission> empty() {
            return new LinkedHashSet<>();
        }
    }

    private static String providerNames() {
        StringBuilder sb = new StringBuilder();
        for (java.security.Provider p : java.security.Security.getProviders()) {
            if (sb.length() > 0) sb.append(',');
            sb.append(p.getName());
        }
        return sb.toString();
    }

    static void mkdirs(File dir) {
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
        dir.setReadable(false, false);
        dir.setWritable(false, false);
        dir.setExecutable(false, false);
        dir.setReadable(true, true);
        dir.setWritable(true, true);
        dir.setExecutable(true, true);
    }

    private static void writePrivate(File f, String content) throws Exception {
        try (FileOutputStream out = new FileOutputStream(f)) {
            out.write(content.getBytes("UTF-8"));
        }
        f.setReadable(false, false);
        f.setWritable(false, false);
        f.setExecutable(false, false);
        f.setReadable(true, true);
        f.setWritable(true, true);
    }

    private static String read(File f) throws Exception {
        if (!f.isFile()) throw new java.io.FileNotFoundException(f.getPath());
        try (InputStream in = new FileInputStream(f)) {
            StringWriter w = new StringWriter();
            byte[] buf = new byte[4096];
            int n;
            while ((n = in.read(buf)) > 0) {
                w.write(new String(buf, 0, n, "UTF-8"));
            }
            return w.toString();
        }
    }

    private static String env(String key, String dflt) {
        try {
            String v = System.getenv(key);
            return v == null ? dflt : v;
        } catch (Throwable t) {
            return dflt;
        }
    }

    private void check(String name, boolean ok, String detail) {
        if (ok) {
            pass++;
        } else {
            fail++;
        }
        String line = "{\"check\":\"" + esc(name) + "\",\"ok\":" + ok
                + ",\"detail\":\"" + esc(detail == null ? "" : detail) + "\"}";
        lines.add(line);
        System.out.println(TAG + " " + line);
        System.out.flush();
        System.out.flush();
    }

    private void finish(String mode) {
        String summary = "{\"activity\":\"MainActivity\",\"mode\":\"" + esc(mode)
                + "\",\"pass\":" + pass + ",\"fail\":" + fail + "}";
        System.out.println(TAG + " " + summary);
        System.out.flush();
        try {
            File dir = new File(dataDir, "keystore-probe");
            mkdirs(dir);
            File out = new File(dir, mode + ".jsonl");
            StringBuilder sb = new StringBuilder();
            for (String l : lines) sb.append(l).append('\n');
            sb.append(summary).append('\n');
            try (FileOutputStream o = new FileOutputStream(out)) {
                o.write(sb.toString().getBytes("UTF-8"));
            }
            out.setReadable(false, false);
            out.setWritable(false, false);
            out.setReadable(true, true);
            out.setWritable(true, true);
        } catch (Throwable t) {
            System.out.println(TAG + " WARN cannot write result: " + t);
        }
    }

    private static String esc(String s) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"':
                    sb.append("\\\"");
                    break;
                case '\\':
                    sb.append("\\\\");
                    break;
                case '\n':
                    sb.append("\\n");
                    break;
                case '\r':
                    sb.append("\\r");
                    break;
                case '\t':
                    sb.append("\\t");
                    break;
                default:
                    if (c < 0x20) {
                        sb.append(String.format("\\u%04x", (int) c));
                    } else {
                        sb.append(c);
                    }
            }
        }
        return sb.toString();
    }
}
