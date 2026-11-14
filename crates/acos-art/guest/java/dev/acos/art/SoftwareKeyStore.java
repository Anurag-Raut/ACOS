package dev.acos.art;

import android.security.keystore.KeyGenParameterSpec;
import android.util.Base64;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.math.BigInteger;
import java.security.AlgorithmParameters;
import java.security.InvalidAlgorithmParameterException;
import java.security.InvalidKeyException;
import java.security.InvalidParameterException;
import java.security.Key;
import java.security.KeyFactory;
import java.security.KeyFactorySpi;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyPairGeneratorSpi;
import java.security.KeyStore;
import java.security.KeyStoreException;
import java.security.KeyStoreSpi;
import java.security.MessageDigest;
import java.security.PrivateKey;
import java.security.Provider;
import java.security.ProviderException;
import java.security.PublicKey;
import java.security.SecureRandom;
import java.security.Security;
import java.security.Signature;
import java.security.cert.Certificate;
import java.security.cert.CertificateFactory;
import java.security.spec.AlgorithmParameterSpec;
import java.security.spec.ECGenParameterSpec;
import java.security.spec.KeySpec;
import java.security.spec.PKCS8EncodedKeySpec;
import java.security.spec.X509EncodedKeySpec;
import java.util.Collections;
import java.util.Date;
import java.util.Enumeration;
import java.util.LinkedHashMap;
import java.util.Properties;
import java.util.Vector;
import javax.crypto.Cipher;
import javax.crypto.CipherSpi;
import javax.crypto.KeyGeneratorSpi;
import javax.crypto.Mac;
import javax.crypto.MacSpi;
import javax.crypto.SecretKey;
import javax.crypto.SecretKeyFactory;
import javax.crypto.SecretKeyFactorySpi;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * A software {@code AndroidKeyStore} provider for the ACOS ART guest.
 *
 * <p>On a device the provider is installed once by the zygote
 * ({@code com.android.internal.os.ZygoteInit.warmUpJcaProviders()}:
 * {@code AndroidKeyStoreProvider.install()}), so every app process inherits it.
 * ACOS starts {@code dalvikvm64} directly, not through the zygote, so
 * {@code Security.getProvider("AndroidKeyStore")} is empty and
 * {@code KeyStore.getInstance("AndroidKeyStore")} throws
 * {@code KeyStoreException: AndroidKeyStore not found}. That exception is what
 * killed {@code io.element.android.x} and {@code com.dot.gallery} in
 * {@code MainActivity.onCreate}/{@code Application.onCreate}.
 *
 * <p>What this installs is a <b>software</b> keystore whose bytes live only in
 * the session's private app-data directory
 * ({@code /data/app-data/<package>/acos-keystore}). It is honest about its
 * security level by construction:
 *
 * <ul>
 *   <li>private/secret keys are {@link SoftwareKey} handles that are
 *       <b>opaque</b>, exactly like AOSP's {@code AndroidKeyStoreKey}:
 *       {@code getEncoded()} and {@code getFormat()} return {@code null}, so
 *       no other JCA provider can obtain the bytes and perform an operation
 *       this store refused. The material is reachable only inside this class,
 *       by alias, from the entry map. Public keys stay ordinary X.509 public
 *       keys (AOSP does the same);</li>
 *   <li>{@code KeyInfo} is answered with {@code securityLevel =
 *       SECURITY_LEVEL_SOFTWARE}, {@code origin}, key size, purposes,
 *       digests/paddings/block modes, validity window and alias, and
 *       {@code isInsideSecureHardware() == false}. Nothing claims hardware
 *       backing;</li>
 *   <li>requested key characteristics are <b>enforced at use</b>: a key
 *       generated for signing refuses encryption, an expired key refuses every
 *       operation, a not-yet-valid key refuses every operation, and a
 *       caller-supplied IV is refused when randomised encryption is required.
 *       The enforcement layer sits in a {@link GuardProvider} inserted at
 *       provider position 1 (the position AOSP gives
 *       {@code AndroidKeyStoreBCWorkaroundProvider}); it only inspects keys
 *       handed out by this store and delegates everything else unchanged;</li>
 *   <li>attestation is <b>refused explicitly</b>
 *       ({@code UNSUPPORTED_ATTESTATION: ...}) instead of returning a
 *       fabricated chain: a software key cannot make an attestation
 *       statement;</li>
 *   <li>requests that cannot be honoured (user authentication, StrongBox,
 *       unlocked-device-bound keys) are rejected with
 *       {@link InvalidAlgorithmParameterException}, never silently
 *       downgraded;</li>
 *   <li>the store file is mode 0600 and its directory 0700; a persistence
 *       failure propagates as {@link ProviderException}/ {@link
 *       KeyStoreException} instead of logging and pretending success;</li>
 *   <li>every session gets a fresh, private directory (the run root is deleted
 *       when the session ends), so keys never leave the sandbox and are never
 *       shared between sessions or apps. ACOS relaunches an app with an empty
 *       app-data directory, so a relaunch is a reset.</li>
 * </ul>
 *
 * <p>AOSP references for the behaviour reproduced here (API 34):
 * <ul>
 *   <li>{@code frameworks/base/keystore/java/android/security/keystore/AndroidKeyStoreProvider.java}
 *       — install position, {@code KeyFactory}/{@code SecretKeyFactory} SPI per
 *       algorithm, {@code getKeySpec(…, KeyInfo.class)};</li>
 *   <li>{@code …/AndroidKeyStoreSecretKeyFactorySpi.java} and
 *       {@code …/AndroidKeyStoreKeyFactorySpi.java} — the only supported key
 *       spec is {@code KeyInfo};</li>
 *   <li>{@code …/KeyInfo.java} — the 22-field constructor used by the SPIs
 *       (its exact descriptor was read out of this image's
 *       {@code framework/classes3.dex} with {@code dexdump});</li>
 *   <li>{@code …/KeyGenParameterSpec.java} — purposes/digests/paddings/modes,
 *       validity window, {@code randomizedEncryptionRequired}, attestation
 *       challenge;</li>
 *   <li>{@code system/security/keystore2} — purpose and validity checks happen
 *       per operation ({@code ErrorCode::KEY_EXPIRED},
 *       {@code ErrorCode::INCOMPATIBLE_PURPOSE}); the Java layer surfaces them
 *       as {@link android.security.keystore.KeyExpiredException} /
 *       {@link java.security.InvalidKeyException};</li>
 *   <li>{@code …/AndroidKeyStoreSpi.java} — generated asymmetric keys get a
 *       self-signed certificate, {@code setEntry} rejects protection parameters
 *       that are not {@code KeyStoreParameter}.</li>
 * </ul>
 */
public final class SoftwareKeyStore extends Provider {
    private static final long serialVersionUID = 1L;

    static final String NAME = "AndroidKeyStore";
    /** Set by {@link #install(String)}; the session's private storage. */
    private static File sDir;
    private static boolean sInstalled;

    private SoftwareKeyStore() {
        super(NAME, 2.0, "ACOS session-private software keystore");
        put("KeyStore." + NAME, Store.class.getName());
        put("KeyGenerator.AES", AesKeyGen.class.getName());
        put("KeyGenerator.HmacSHA256", HmacSHA256KeyGen.class.getName());
        put("KeyGenerator.HmacSHA1", HmacSHA1KeyGen.class.getName());
        put("KeyPairGenerator.RSA", RsaKeyPairGen.class.getName());
        put("KeyPairGenerator.EC", EcKeyPairGen.class.getName());
        // AOSP installs one SecretKeyFactory/KeyFactory SPI per algorithm whose
        // only job is to answer KeyInfo for a key of this keystore.
        put("SecretKeyFactory.AES", SecretKeyInfoFactory.class.getName());
        put("SecretKeyFactory.HmacSHA256", HmacSHA256InfoFactory.class.getName());
        put("SecretKeyFactory.HmacSHA1", HmacSHA1InfoFactory.class.getName());
        put("KeyFactory.RSA", RsaKeyInfoFactory.class.getName());
        put("KeyFactory.EC", EcKeyInfoFactory.class.getName());
    }

    /**
     * Install the provider for this guest process, storing keys under
     * {@code <dataDir>/acos-keystore}. Idempotent. If a provider with the same
     * name is already present (a zygote would have installed the real one) it
     * is left alone.
     */
    public static synchronized void install(String dataDir) {
        if (sInstalled) return;
        File dir = new File(dataDir, "acos-keystore");
        mkdirsPrivate(dir);
        sDir = dir;
        if (Security.getProvider(NAME) == null) {
            Security.addProvider(new SoftwareKeyStore());
            // AOSP's key operations are serviced by
            // AndroidKeyStoreBCWorkaroundProvider, which is installed ahead of
            // the general-purpose providers so a keystore key never reaches
            // Conscrypt directly. Same position here, same reason: the purpose
            // and validity checks have to run before any crypto does.
            Security.insertProviderAt(new GuardProvider(), 1);
            System.out.println("ACOSLAUNCH MARK keystore provider=software dir=" + dir);
        } else {
            System.out.println("ACOSLAUNCH MARK keystore provider=present dir=" + dir);
        }
        sInstalled = true;
    }

    /**
     * What this provider is and is not, in one line per fact. Printed by the
     * probe's `fidelity` mode so the limits travel with the evidence.
     */
    static String describeLimits() {
        return "software-keystore compatibility; keys are exportable in-process; "
                + "no TEE, no StrongBox, no attestation, no user-auth binding";
    }

    private static synchronized File dir() {
        if (sDir == null) {
            // Defensive: install() normally ran before any app code. A provider
            // without a directory would silently lose keys, so keep it inside
            // the session's private data root.
            sDir = new File("/data/app-data/acos-keystore");
            mkdirsPrivate(sDir);
        }
        return sDir;
    }

    /** {@code mkdir -p} + 0700 (a keystore directory is owner-only). */
    private static void mkdirsPrivate(File dir) {
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
        if (!dir.isDirectory()) {
            throw new ProviderException("ACOS keystore: cannot create " + dir);
        }
        dir.setReadable(false, false);
        dir.setWritable(false, false);
        dir.setExecutable(false, false);
        dir.setReadable(true, true);
        dir.setWritable(true, true);
        dir.setExecutable(true, true);
    }

    // ---------------------------------------------------------------- entries

    /** One stored alias: a secret key, a key pair or a certificate. */
    private static final class Entry {
        String type; // "secret" | "private" | "certificate"
        String alg = "";
        byte[] material = new byte[0];
        byte[] publicMaterial = new byte[0];
        byte[][] chain = new byte[0][];
        int keySize;
        int purposes;
        int origin = KeyPropertiesConstants.ORIGIN_GENERATED;
        /** 0 = unbounded (AOSP reports {@code null} for those). */
        long validStart;
        long validEnd;
        String[] blockModes = new String[0];
        String[] encPaddings = new String[0];
        String[] sigPaddings = new String[0];
        String[] digests = new String[0];
        boolean randomizedEncryptionRequired = true;
        long createdAt;
    }

    /**
     * {@code KeyProperties} values this provider uses. Declared locally because
     * the class is `@hide`-trimmed in `android.jar`; the values are the
     * documented public constants.
     */
    private static final class KeyPropertiesConstants {
        static final int PURPOSE_ENCRYPT = 1;
        static final int PURPOSE_DECRYPT = 2;
        static final int PURPOSE_SIGN = 4;
        static final int PURPOSE_VERIFY = 8;
        static final int PURPOSE_WRAP_KEY = 32;
        static final int PURPOSE_AGREE_KEY = 64;
        static final int PURPOSE_ATTEST_KEY = 128;
        static final int ORIGIN_GENERATED = 1;
        static final int ORIGIN_IMPORTED = 2;
        static final int SECURITY_LEVEL_SOFTWARE = 0;
        static final String DIGEST_SHA256 = "SHA-256";
        static final String DIGEST_SHA1 = "SHA-1";
        static final String BLOCK_MODE_CBC = "CBC";
        static final String BLOCK_MODE_ECB = "ECB";
        static final String BLOCK_MODE_CTR = "CTR";
        static final String BLOCK_MODE_GCM = "GCM";
        static final String PADDING_NONE = "NoPadding";
        static final String PADDING_PKCS7 = "PKCS7Padding";
        static final String PADDING_PKCS1 = "PKCS1Padding";
        static final String PADDING_OAEP = "OAEPPadding";
    }

    // Static so every KeyStore instance a process creates sees the same store.
    private static final LinkedHashMap<String, Entry> ENTRIES = new LinkedHashMap<>();
    private static boolean sLoaded;

    private static synchronized void ensureLoaded() {
        if (sLoaded) return;
        sLoaded = true;
        File f = new File(dir(), "entries.properties");
        if (!f.isFile()) return;
        Properties p = new Properties();
        try (InputStream in = new FileInputStream(f)) {
            p.load(in);
        } catch (Throwable t) {
            throw new ProviderException("ACOS keystore: cannot read " + f + ": " + t);
        }
        for (String k : p.stringPropertyNames()) {
            if (!k.endsWith(".type")) continue;
            String alias = k.substring(0, k.length() - ".type".length());
            Entry e = new Entry();
            e.type = p.getProperty(k);
            e.alg = p.getProperty(alias + ".alg", "");
            e.material = decode(p.getProperty(alias + ".material"));
            e.publicMaterial = decode(p.getProperty(alias + ".public"));
            e.keySize = parseInt(p.getProperty(alias + ".keysize"), 0);
            e.purposes = parseInt(p.getProperty(alias + ".purposes"), 0);
            e.origin = parseInt(p.getProperty(alias + ".origin"),
                    KeyPropertiesConstants.ORIGIN_GENERATED);
            e.validStart = parseLong(p.getProperty(alias + ".validstart"), 0);
            e.validEnd = parseLong(p.getProperty(alias + ".validend"), 0);
            e.blockModes = split(p.getProperty(alias + ".blockmodes"));
            e.encPaddings = split(p.getProperty(alias + ".encpaddings"));
            e.sigPaddings = split(p.getProperty(alias + ".sigpaddings"));
            e.digests = split(p.getProperty(alias + ".digests"));
            e.randomizedEncryptionRequired =
                    !"false".equals(p.getProperty(alias + ".randomized"));
            e.createdAt = parseLong(p.getProperty(alias + ".created"), 0);
            String chain = p.getProperty(alias + ".chain", "");
            if (chain.length() > 0) {
                String[] parts = chain.split(",");
                e.chain = new byte[parts.length][];
                for (int i = 0; i < parts.length; i++) e.chain[i] = decode(parts[i]);
            }
            ENTRIES.put(alias, e);
        }
    }

    private static int parseInt(String s, int dflt) {
        try {
            return s == null ? dflt : Integer.parseInt(s.trim());
        } catch (NumberFormatException e) {
            return dflt;
        }
    }

    private static long parseLong(String s, long dflt) {
        try {
            return s == null ? dflt : Long.parseLong(s.trim());
        } catch (NumberFormatException e) {
            return dflt;
        }
    }

    private static String[] split(String s) {
        if (s == null || s.length() == 0) return new String[0];
        return s.split(",");
    }

    private static String join(String[] a) {
        if (a == null || a.length == 0) return "";
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < a.length; i++) {
            if (i > 0) sb.append(',');
            sb.append(a[i]);
        }
        return sb.toString();
    }

    private static byte[] decode(String s) {
        if (s == null || s.length() == 0) return new byte[0];
        try {
            return Base64.decode(s, Base64.NO_WRAP);
        } catch (Throwable t) {
            return new byte[0];
        }
    }

    private static String encode(byte[] b) {
        if (b == null || b.length == 0) return "";
        return Base64.encodeToString(b, Base64.NO_WRAP);
    }

    /**
     * Write the store and return normally only when the bytes really are on
     * disk with owner-only permissions. A failure is a failure: the K5 advisor
     * called out log-and-succeed persistence explicitly.
     */
    private static synchronized void persist() {
        ensureLoaded();
        Properties p = new Properties();
        for (java.util.Map.Entry<String, Entry> me : ENTRIES.entrySet()) {
            String alias = me.getKey();
            Entry e = me.getValue();
            p.setProperty(alias + ".type", e.type);
            p.setProperty(alias + ".alg", e.alg);
            p.setProperty(alias + ".material", encode(e.material));
            p.setProperty(alias + ".public", encode(e.publicMaterial));
            p.setProperty(alias + ".keysize", Integer.toString(e.keySize));
            p.setProperty(alias + ".purposes", Integer.toString(e.purposes));
            p.setProperty(alias + ".origin", Integer.toString(e.origin));
            p.setProperty(alias + ".validstart", Long.toString(e.validStart));
            p.setProperty(alias + ".validend", Long.toString(e.validEnd));
            p.setProperty(alias + ".blockmodes", join(e.blockModes));
            p.setProperty(alias + ".encpaddings", join(e.encPaddings));
            p.setProperty(alias + ".sigpaddings", join(e.sigPaddings));
            p.setProperty(alias + ".digests", join(e.digests));
            p.setProperty(alias + ".randomized", Boolean.toString(e.randomizedEncryptionRequired));
            p.setProperty(alias + ".created", Long.toString(e.createdAt));
            StringBuilder chain = new StringBuilder();
            for (int i = 0; i < e.chain.length; i++) {
                if (i > 0) chain.append(',');
                chain.append(encode(e.chain[i]));
            }
            p.setProperty(alias + ".chain", chain.toString());
        }
        File f = new File(dir(), "entries.properties");
        File tmp = new File(dir(), "entries.properties.tmp");
        try (OutputStream out = new FileOutputStream(tmp)) {
            p.store(out, "ACOS session-private software keystore");
        } catch (Throwable t) {
            throw new ProviderException("ACOS keystore: cannot write " + tmp + ": " + t);
        }
        restrictToOwner(tmp);
        if (!tmp.renameTo(f)) {
            //noinspection ResultOfMethodCallIgnored
            f.delete();
            if (!tmp.renameTo(f)) {
                throw new ProviderException("ACOS keystore: cannot install " + f);
            }
        }
        restrictToOwner(f);
        if (!f.isFile() || f.length() == 0) {
            throw new ProviderException("ACOS keystore: " + f + " is missing after write");
        }
    }

    /** 0600: the key material is readable and writable by its owner only. */
    private static void restrictToOwner(File f) {
        f.setReadable(false, false);
        f.setWritable(false, false);
        f.setExecutable(false, false);
        f.setReadable(true, true);
        f.setWritable(true, true);
    }

    // ------------------------------------------------------------- key views

    /** A key handed out by this store: only its alias, never its material. */
    public static class SoftwareKey {
        final String alias;
        final boolean secret;

        SoftwareKey(String alias, boolean secret) {
            this.alias = alias;
            this.secret = secret;
        }
    }

    /**
     * A secret key of this store. <b>Opaque</b> like AOSP's
     * {@code AndroidKeyStoreSecretKey}: {@code getEncoded()}/{@code getFormat()}
     * are {@code null}, so `javax.crypto.Cipher`'s delayed provider selection
     * cannot hand the key to Conscrypt/BouncyCastle and have them perform an
     * operation this store refused (the K5b finding). Our own SPI resolves the
     * material by alias ({@link #materialSecret}).
     */
    static final class StoreSecretKey extends SoftwareKey implements SecretKey {
        private final String algorithm;

        StoreSecretKey(String alias, String algorithm) {
            super(alias, true);
            this.algorithm = algorithm == null || algorithm.isEmpty() ? "AES" : algorithm;
        }

        @Override
        public String getAlgorithm() {
            return algorithm;
        }

        @Override
        public String getFormat() {
            return null;
        }

        @Override
        public byte[] getEncoded() {
            return null;
        }
    }

    /** A private key of this store: opaque, see {@link StoreSecretKey}. */
    static final class StorePrivateKey extends SoftwareKey implements PrivateKey {
        private final String algorithm;

        StorePrivateKey(String alias, String algorithm) {
            super(alias, false);
            this.algorithm = algorithm == null || algorithm.isEmpty() ? "RSA" : algorithm;
        }

        @Override
        public String getAlgorithm() {
            return algorithm;
        }

        @Override
        public String getFormat() {
            return null;
        }

        @Override
        public byte[] getEncoded() {
            return null;
        }
    }

    private static SecretKey asSecret(String alias, Entry e) {
        if (e == null || !"secret".equals(e.type)) return null;
        return new StoreSecretKey(alias, e.alg);
    }

    private static PrivateKey asPrivate(String alias, Entry e) {
        if (e == null || !"private".equals(e.type)) return null;
        return new StorePrivateKey(alias, e.alg);
    }

    // ------------------------------------------------------ key material (ours)

    /**
     * The real JCA secret key behind a store key. <b>Only</b> for the SPIs of
     * this provider ({@code GuardCipher}/{@code GuardMac}/{@code
     * GuardSignature}): the material never leaves the process through the
     * {@link Key} interface, which is what keeps a refused operation refused.
     */
    static SecretKey materialSecret(Key key) {
        Entry e = entryOf(key);
        if (e == null || !"secret".equals(e.type)) return null;
        return new SecretKeySpec(e.material, e.alg.length() > 0 ? e.alg : "AES");
    }

    /** The real JCA private key behind a store key; see {@link #materialSecret}. */
    static PrivateKey materialPrivate(Key key) throws InvalidKeyException {
        Entry e = entryOf(key);
        if (e == null || !"private".equals(e.type)) return null;
        try {
            KeyFactory kf = KeyFactory.getInstance(e.alg.length() > 0 ? e.alg : "RSA");
            return kf.generatePrivate(new PKCS8EncodedKeySpec(e.material));
        } catch (Exception ex) {
            throw new InvalidKeyException("Keystore operation failed", ex);
        }
    }

    private static PublicKey asPublic(Entry e) throws Exception {
        if (e == null || e.publicMaterial.length == 0) return null;
        KeyFactory kf = KeyFactory.getInstance(e.alg.length() > 0 ? e.alg : "RSA");
        return kf.generatePublic(new X509EncodedKeySpec(e.publicMaterial));
    }

    private static Certificate[] asChain(Entry e) {
        if (e == null || e.chain.length == 0) return null;
        try {
            CertificateFactory cf = CertificateFactory.getInstance("X.509");
            Certificate[] out = new Certificate[e.chain.length];
            for (int i = 0; i < e.chain.length; i++) {
                out[i] = cf.generateCertificate(new java.io.ByteArrayInputStream(e.chain[i]));
            }
            return out;
        } catch (Throwable t) {
            return null;
        }
    }

    /** The entry behind a key this store handed out (or {@code null}). */
    private static Entry entryOf(Key key) {
        if (!(key instanceof SoftwareKey)) return null;
        synchronized (SoftwareKeyStore.class) {
            ensureLoaded();
            return ENTRIES.get(((SoftwareKey) key).alias);
        }
    }

    // ----------------------------------------------------------- spec checks

    /** The services-level checks every generator shares. */
    private static void checkSpec(KeyGenParameterSpec spec, String algorithm)
            throws InvalidAlgorithmParameterException {
        String alias = spec.getKeystoreAlias();
        if (alias == null || alias.isEmpty()) {
            throw new InvalidAlgorithmParameterException("AndroidKeyStore needs an alias");
        }
        if (spec.isUserAuthenticationRequired()) {
            // A software keystore cannot enforce "unlock with the user"; a key
            // that ignored that requirement would be a security lie.
            throw new InvalidAlgorithmParameterException(
                    "ACOS software keystore cannot enforce user authentication");
        }
        if (spec.isStrongBoxBacked()) {
            throw new InvalidAlgorithmParameterException(
                    "ACOS software keystore has no StrongBox");
        }
        String attestation = attestationChallenge(spec);
        if (attestation != null && !attestation.isEmpty()) {
            // keystore2 answers attestation by asking keymaster for a
            // certificate chain. There is no keymaster here, so a returned
            // chain would be fabricated. Refuse, structured, and cite it.
            throw new InvalidAlgorithmParameterException(
                    "UNSUPPORTED_ATTESTATION: the ACOS software keystore has no "
                            + "attestation-capable keymaster; refusing to fabricate a chain");
        }
        int purposes = purposes(spec);
        if (purposes == 0) {
            throw new InvalidAlgorithmParameterException(
                    "At least one purpose must be specified for a key");
        }
        int unsupported = purposes & ~allowedPurposes(algorithm);
        if (unsupported != 0) {
            throw new InvalidAlgorithmParameterException(
                    "Unsupported purposes 0x" + Integer.toHexString(unsupported)
                            + " for algorithm " + algorithm);
        }
        Date start = keyValidityStart(spec);
        Date end = keyValidityForOriginationEnd(spec);
        if (start != null && end != null && end.before(start)) {
            throw new InvalidAlgorithmParameterException(
                    "Key validity end is before its start");
        }
    }

    private static int allowedPurposes(String algorithm) {
        if ("RSA".equals(algorithm)) {
            return KeyPropertiesConstants.PURPOSE_ENCRYPT | KeyPropertiesConstants.PURPOSE_DECRYPT
                    | KeyPropertiesConstants.PURPOSE_SIGN | KeyPropertiesConstants.PURPOSE_VERIFY
                    | KeyPropertiesConstants.PURPOSE_WRAP_KEY
                    | KeyPropertiesConstants.PURPOSE_ATTEST_KEY;
        }
        if ("EC".equals(algorithm)) {
            return KeyPropertiesConstants.PURPOSE_SIGN | KeyPropertiesConstants.PURPOSE_VERIFY
                    | KeyPropertiesConstants.PURPOSE_AGREE_KEY;
        }
        if (algorithm.startsWith("Hmac")) {
            return KeyPropertiesConstants.PURPOSE_SIGN | KeyPropertiesConstants.PURPOSE_VERIFY;
        }
        return KeyPropertiesConstants.PURPOSE_ENCRYPT | KeyPropertiesConstants.PURPOSE_DECRYPT
                | KeyPropertiesConstants.PURPOSE_WRAP_KEY;
    }

    // `KeyGenParameterSpec`'s getters for the requested characteristics are
    // `@hide` (present in the image, trimmed from `android.jar`), exactly as
    // `AndroidKeyStoreProvider`'s own SPIs reach them. Reflected once, cached.
    //
    // A reflection failure fails *closed*: silently returning null would drop the
    // validity-window, attestation, purposes or randomized-encryption checks and
    // accept a key the app asked us to constrain.
    private static java.lang.reflect.Method specMethod(String name, Class<?>... params) {
        try {
            java.lang.reflect.Method m = KeyGenParameterSpec.class.getMethod(name, params);
            m.setAccessible(true);
            return m;
        } catch (Throwable t) {
            throw new ProviderException(
                    "ACOS keystore: cannot read KeyGenParameterSpec." + name + ": " + t, t);
        }
    }

    private static Object call(Object target, String name) {
        java.lang.reflect.Method m = specMethod(name);
        try {
            return m.invoke(target);
        } catch (java.lang.reflect.InvocationTargetException e) {
            Throwable cause = e.getCause() != null ? e.getCause() : e;
            // `KeyGenParameterSpec`'s getters throw `IllegalStateException("<X> not
            // specified")` when the characteristic was never set (AOSP's own
            // contract, e.g. `getDigests`). That is the getter working, not a
            // reflection failure, so it keeps its old meaning: not specified ->
            // null -> the caller's default.
            if (cause instanceof IllegalStateException) {
                return null;
            }
            throw new ProviderException(
                    "ACOS keystore: cannot read KeyGenParameterSpec." + name + ": " + cause,
                    cause);
        } catch (Throwable t) {
            throw new ProviderException(
                    "ACOS keystore: cannot read KeyGenParameterSpec." + name + ": " + t, t);
        }
    }

    private static int purposes(KeyGenParameterSpec spec) {
        Object v = call(spec, "getPurposes");
        return v instanceof Integer ? (Integer) v : 0;
    }

    private static String attestationChallenge(KeyGenParameterSpec spec) {
        Object v = call(spec, "getAttestationChallenge");
        return v instanceof byte[] ? new String((byte[]) v) : null;
    }

    private static Date keyValidityStart(KeyGenParameterSpec spec) {
        Object v = call(spec, "getKeyValidityStart");
        return v instanceof Date ? (Date) v : null;
    }

    private static Date keyValidityForOriginationEnd(KeyGenParameterSpec spec) {
        Object v = call(spec, "getKeyValidityForOriginationEnd");
        return v instanceof Date ? (Date) v : null;
    }

    private static Date keyValidityForConsumptionEnd(KeyGenParameterSpec spec) {
        Object v = call(spec, "getKeyValidityForConsumptionEnd");
        return v instanceof Date ? (Date) v : null;
    }

    private static boolean randomizedEncryptionRequired(KeyGenParameterSpec spec) {
        Object v = call(spec, "isRandomizedEncryptionRequired");
        return !(v instanceof Boolean) || (Boolean) v;
    }

    private static String[] blockModes(KeyGenParameterSpec spec) {
        Object v = call(spec, "getBlockModes");
        return v instanceof String[] ? (String[]) v : new String[0];
    }

    private static String[] encPaddings(KeyGenParameterSpec spec) {
        Object v = call(spec, "getEncryptionPaddings");
        return v instanceof String[] ? (String[]) v : new String[0];
    }

    private static String[] sigPaddings(KeyGenParameterSpec spec) {
        Object v = call(spec, "getSignaturePaddings");
        return v instanceof String[] ? (String[]) v : new String[0];
    }

    private static String[] digests(KeyGenParameterSpec spec) {
        Object v = call(spec, "getDigests");
        return v instanceof String[] ? (String[]) v : new String[0];
    }

    private static int keySizeOf(PublicKey key, int fallback) {
        try {
            if (key instanceof java.security.interfaces.RSAPublicKey) {
                return ((java.security.interfaces.RSAPublicKey) key).getModulus().bitLength();
            }
            if (key instanceof java.security.interfaces.ECPublicKey) {
                return ((java.security.interfaces.ECPublicKey) key).getParams()
                        .getCurve().getField().getFieldSize();
            }
        } catch (Throwable ignored) {
        }
        return fallback;
    }

    private static void store(String alias, Entry e) {
        synchronized (SoftwareKeyStore.class) {
            ensureLoaded();
            ENTRIES.put(alias, e);
            persist();
        }
    }

    private static Entry newEntry(String type, String alg, byte[] material) {
        Entry e = new Entry();
        e.type = type;
        e.alg = alg;
        e.material = material;
        e.createdAt = System.currentTimeMillis();
        return e;
    }

    // ------------------------------------------------------------- KeyStoreSpi

    /** {@code KeyStoreSpi} over the session-private property file. */
    public static final class Store extends KeyStoreSpi {
        @Override
        public Key engineGetKey(String alias, char[] password) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                if (e == null) return null;
                try {
                    if ("secret".equals(e.type)) return asSecret(alias, e);
                    if ("private".equals(e.type)) return asPrivate(alias, e);
                } catch (Throwable t) {
                    throw new ProviderException("ACOS keystore: cannot decode " + alias + ": " + t);
                }
                return null;
            }
        }

        @Override
        public Certificate[] engineGetCertificateChain(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                return asChain(ENTRIES.get(alias));
            }
        }

        @Override
        public Certificate engineGetCertificate(String alias) {
            Certificate[] c = engineGetCertificateChain(alias);
            return c != null && c.length > 0 ? c[0] : null;
        }

        @Override
        public Date engineGetCreationDate(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                if (e == null || e.createdAt == 0) return null;
                return new Date(e.createdAt);
            }
        }

        @Override
        public void engineSetKeyEntry(String alias, Key key, char[] password, Certificate[] chain)
                throws KeyStoreException {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                if (key == null) throw new KeyStoreException("null key");
                if (key instanceof SoftwareKey) {
                    // Our own keys are opaque: they have no encoded form to
                    // store, and AOSP refuses the same way.
                    throw new KeyStoreException("Cannot store a key that belongs to "
                            + "AndroidKeyStore: " + key.getClass().getName());
                }
                byte[] encoded = key.getEncoded();
                if (encoded == null) {
                    throw new KeyStoreException("Key has no encoded form: "
                            + key.getClass().getName());
                }
                Entry e = newEntry("", key.getAlgorithm(), encoded);
                e.origin = KeyPropertiesConstants.ORIGIN_IMPORTED;
                e.purposes = allowedPurposes(key.getAlgorithm());
                if (key instanceof SecretKey) {
                    e.type = "secret";
                    e.keySize = e.material.length * 8;
                } else if (key instanceof PrivateKey) {
                    e.type = "private";
                    if (chain == null || chain.length == 0) {
                        // keystore2 cannot store a private key without the
                        // certificate that identifies it.
                        throw new KeyStoreException(
                                "Certificate chain not provided for the private key");
                    }
                    e.chain = new byte[chain.length][];
                    for (int i = 0; i < chain.length; i++) {
                        try {
                            e.chain[i] = chain[i].getEncoded();
                        } catch (Throwable t) {
                            throw new KeyStoreException("certificate encoding failed");
                        }
                    }
                    try {
                        CertificateFactory cf = CertificateFactory.getInstance("X.509");
                        PublicKey pub = cf.generateCertificate(
                                new java.io.ByteArrayInputStream(e.chain[0])).getPublicKey();
                        e.keySize = keySizeOf(pub, 0);
                    } catch (Throwable ignored) {
                        e.keySize = 0;
                    }
                } else {
                    throw new KeyStoreException("unsupported key " + key.getClass());
                }
                store(alias, e);
            }
        }

        @Override
        public void engineSetKeyEntry(String alias, byte[] key, Certificate[] chain)
                throws KeyStoreException {
            throw new KeyStoreException("encoded keys are not supported");
        }

        @Override
        public void engineSetCertificateEntry(String alias, Certificate cert)
                throws KeyStoreException {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = newEntry("certificate", "", new byte[0]);
                try {
                    e.material = cert.getEncoded();
                } catch (Throwable t) {
                    throw new KeyStoreException("certificate encoding failed");
                }
                e.origin = KeyPropertiesConstants.ORIGIN_IMPORTED;
                store(alias, e);
            }
        }

        @Override
        public void engineDeleteEntry(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                if (ENTRIES.remove(alias) != null) persist();
            }
        }

        @Override
        public Enumeration<String> engineAliases() {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                return new Vector<>(ENTRIES.keySet()).elements();
            }
        }

        @Override
        public boolean engineContainsAlias(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                return ENTRIES.containsKey(alias);
            }
        }

        @Override
        public int engineSize() {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                return ENTRIES.size();
            }
        }

        @Override
        public boolean engineIsKeyEntry(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                return e != null && !"certificate".equals(e.type);
            }
        }

        @Override
        public String engineGetCertificateAlias(Certificate cert) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                if (cert == null) return null;
                try {
                    byte[] want = cert.getEncoded();
                    for (java.util.Map.Entry<String, Entry> me : ENTRIES.entrySet()) {
                        Entry e = me.getValue();
                        if ("certificate".equals(e.type)
                                && java.util.Arrays.equals(want, e.material)) {
                            return me.getKey();
                        }
                    }
                } catch (Throwable ignored) {
                }
                return null;
            }
        }

        @Override
        public boolean engineIsCertificateEntry(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                return e != null && "certificate".equals(e.type);
            }
        }

        @Override
        public KeyStore.Entry engineGetEntry(String alias, KeyStore.ProtectionParameter protParam) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                if (e == null) return null;
                try {
                    if ("secret".equals(e.type)) {
                        return new KeyStore.SecretKeyEntry(asSecret(alias, e));
                    }
                    if ("private".equals(e.type)) {
                        Certificate[] chain = asChain(e);
                        return new KeyStore.PrivateKeyEntry(
                                asPrivate(alias, e),
                                chain != null ? chain : new Certificate[0]);
                    }
                } catch (Throwable t) {
                    throw new ProviderException("ACOS keystore: cannot read " + alias + ": " + t);
                }
                return null;
            }
        }

        /**
         * AOSP's {@code AndroidKeyStoreSpi.engineSetEntry} accepts only a
         * {@code KeyStoreParameter} (or none); anything else is a caller error,
         * not something to ignore.
         */
        @Override
        public void engineSetEntry(String alias, KeyStore.Entry entry,
                KeyStore.ProtectionParameter protParam) throws KeyStoreException {
            if (protParam != null
                    && !"android.security.KeyStoreParameter"
                            .equals(protParam.getClass().getName())) {
                throw new KeyStoreException("Unsupported protection parameter class:"
                        + protParam.getClass().getName()
                        + ". Supported: android.security.KeyStoreParameter");
            }
            if (entry instanceof KeyStore.SecretKeyEntry) {
                SecretKey k = ((KeyStore.SecretKeyEntry) entry).getSecretKey();
                engineSetKeyEntry(alias, k, null, null);
            } else if (entry instanceof KeyStore.PrivateKeyEntry) {
                KeyStore.PrivateKeyEntry p = (KeyStore.PrivateKeyEntry) entry;
                engineSetKeyEntry(alias, p.getPrivateKey(), null, p.getCertificateChain());
            } else if (entry instanceof KeyStore.TrustedCertificateEntry) {
                engineSetCertificateEntry(alias,
                        ((KeyStore.TrustedCertificateEntry) entry).getTrustedCertificate());
            } else {
                throw new KeyStoreException("unsupported entry " + entry.getClass());
            }
        }

        @Override
        public boolean engineEntryInstanceOf(String alias, Class<? extends KeyStore.Entry> entryClass) {
            if (entryClass == null) return false;
            if (entryClass.isAssignableFrom(KeyStore.SecretKeyEntry.class)) {
                return "secret".equals(typeOf(alias));
            }
            if (entryClass.isAssignableFrom(KeyStore.PrivateKeyEntry.class)) {
                return "private".equals(typeOf(alias));
            }
            return false;
        }

        private static String typeOf(String alias) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                return e == null ? null : e.type;
            }
        }

        @Override
        public void engineLoad(InputStream stream, char[] password) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
            }
        }

        @Override
        public void engineLoad(KeyStore.LoadStoreParameter param) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
            }
        }

        @Override
        public void engineStore(OutputStream stream, char[] password) {
            persist();
        }

        @Override
        public void engineStore(KeyStore.LoadStoreParameter param) {
            persist();
        }
    }

    // ------------------------------------------------------------- generators

    private abstract static class Gen extends KeyGeneratorSpi {
        private String alias;
        private int keySize;
        private SecureRandom random = new SecureRandom();
        private KeyGenParameterSpec spec;

        /** The JCA algorithm name this generator produces. */
        abstract String jcaAlgorithm();

        @Override
        protected void engineInit(SecureRandom r) {
            if (r != null) random = r;
        }

        @Override
        protected void engineInit(int keysize, SecureRandom r) throws InvalidParameterException {
            keySize = keysize;
            if (r != null) random = r;
        }

        @Override
        protected void engineInit(AlgorithmParameterSpec params, SecureRandom r)
                throws InvalidAlgorithmParameterException {
            if (!(params instanceof KeyGenParameterSpec)) {
                throw new InvalidAlgorithmParameterException(
                        "AndroidKeyStore requires a KeyGenParameterSpec");
            }
            KeyGenParameterSpec s = (KeyGenParameterSpec) params;
            checkSpec(s, jcaAlgorithm());
            spec = s;
            alias = s.getKeystoreAlias();
            if (s.getKeySize() > 0) keySize = s.getKeySize();
            if (r != null) random = r;
        }

        @Override
        protected SecretKey engineGenerateKey() {
            int bits = keySize > 0 ? keySize : 256;
            if (bits < 8 || bits % 8 != 0 || bits > 8192) {
                throw new InvalidParameterException("bad key size " + bits);
            }
            byte[] material = new byte[bits / 8];
            random.nextBytes(material);
            SecretKeySpec delegate = new SecretKeySpec(material, jcaAlgorithm());
            if (alias != null) {
                Entry e = newEntry("secret", jcaAlgorithm(), material);
                e.keySize = bits;
                e.purposes = spec != null ? purposes(spec)
                        : allowedPurposes(jcaAlgorithm());
                e.origin = KeyPropertiesConstants.ORIGIN_GENERATED;
                if (spec != null) {
                    Date start = keyValidityStart(spec);
                    Date origEnd = keyValidityForOriginationEnd(spec);
                    Date consEnd = keyValidityForConsumptionEnd(spec);
                    e.validStart = start != null ? start.getTime() : 0;
                    long end = 0;
                    if (origEnd != null) end = origEnd.getTime();
                    if (consEnd != null) end = end == 0 ? consEnd.getTime() : Math.min(end, consEnd.getTime());
                    e.validEnd = end;
                    e.blockModes = blockModes(spec);
                    e.encPaddings = encPaddings(spec);
                    e.sigPaddings = sigPaddings(spec);
                    e.digests = digests(spec);
                    e.randomizedEncryptionRequired = randomizedEncryptionRequired(spec);
                } else {
                    e.blockModes = new String[] {KeyPropertiesConstants.BLOCK_MODE_GCM,
                            KeyPropertiesConstants.BLOCK_MODE_CBC,
                            KeyPropertiesConstants.BLOCK_MODE_CTR,
                            KeyPropertiesConstants.BLOCK_MODE_ECB};
                    e.encPaddings = new String[] {KeyPropertiesConstants.PADDING_NONE,
                            KeyPropertiesConstants.PADDING_PKCS7};
                }
                store(alias, e);
                return new StoreSecretKey(alias, e.alg);
            }
            return delegate;
        }
    }

    /** {@code KeyGenerator} for {@code AES}; the alias becomes a store entry. */
    public static final class AesKeyGen extends Gen {
        @Override
        String jcaAlgorithm() {
            return "AES";
        }
    }

    /** {@code KeyGenerator} for {@code HmacSHA256}. */
    public static final class HmacSHA256KeyGen extends Gen {
        @Override
        String jcaAlgorithm() {
            return "HmacSHA256";
        }
    }

    /** {@code KeyGenerator} for {@code HmacSHA1}. */
    public static final class HmacSHA1KeyGen extends Gen {
        @Override
        String jcaAlgorithm() {
            return "HmacSHA1";
        }
    }

    /**
     * {@code KeyPairGenerator} for RSA/EC. The pair is produced by the
     * platform's default (software) JCA provider, the private half is kept
     * under the alias and a self-signed certificate is generated for it — the
     * shape {@code AndroidKeyStoreSpi} gives a generated key pair
     * ({@code getCertificateChain(alias).length == 1}).
     */
    public abstract static class KeyPairGen extends KeyPairGeneratorSpi {
        /** The JCA key algorithm (`RSA`/`EC`); each service has its own class. */
        abstract String keyAlgorithm();

        private String alias;
        private int keySize;
        private ECGenParameterSpec ecParams;
        private SecureRandom random = new SecureRandom();
        private KeyGenParameterSpec spec;

        @Override
        public void initialize(int keysize, SecureRandom r) {
            keySize = keysize;
            if (r != null) random = r;
        }

        @Override
        public void initialize(AlgorithmParameterSpec params, SecureRandom r)
                throws InvalidAlgorithmParameterException {
            if (!(params instanceof KeyGenParameterSpec)) {
                throw new InvalidAlgorithmParameterException(
                        "AndroidKeyStore requires a KeyGenParameterSpec");
            }
            KeyGenParameterSpec s = (KeyGenParameterSpec) params;
            checkSpec(s, keyAlgorithm());
            spec = s;
            alias = s.getKeystoreAlias();
            if (s.getKeySize() > 0) keySize = s.getKeySize();
            AlgorithmParameterSpec inner = s.getAlgorithmParameterSpec();
            if (inner instanceof ECGenParameterSpec) ecParams = (ECGenParameterSpec) inner;
            if (r != null) random = r;
        }

        @Override
        public KeyPair generateKeyPair() {
            String algorithm = keyAlgorithm();
            int bits = keySize > 0 ? keySize : ("EC".equals(algorithm) ? 256 : 2048);
            try {
                KeyPairGenerator kpg;
                if (ecParams != null) {
                    kpg = KeyPairGenerator.getInstance("EC");
                    kpg.initialize(ecParams, random);
                } else {
                    kpg = KeyPairGenerator.getInstance(algorithm);
                    kpg.initialize(bits, random);
                }
                KeyPair kp = kpg.generateKeyPair();
                if (alias == null) return kp;
                Entry e = newEntry("private", kp.getPrivate().getAlgorithm(),
                        kp.getPrivate().getEncoded());
                e.publicMaterial = kp.getPublic().getEncoded();
                e.keySize = keySizeOf(kp.getPublic(), bits);
                e.purposes = spec != null ? purposes(spec) : allowedPurposes(algorithm);
                e.origin = KeyPropertiesConstants.ORIGIN_GENERATED;
                if (spec != null) {
                    Date start = keyValidityStart(spec);
                    Date origEnd = keyValidityForOriginationEnd(spec);
                    Date consEnd = keyValidityForConsumptionEnd(spec);
                    e.validStart = start != null ? start.getTime() : 0;
                    long end = 0;
                    if (origEnd != null) end = origEnd.getTime();
                    if (consEnd != null) {
                        end = end == 0 ? consEnd.getTime() : Math.min(end, consEnd.getTime());
                    }
                    e.validEnd = end;
                    e.blockModes = blockModes(spec);
                    e.encPaddings = encPaddings(spec);
                    e.sigPaddings = sigPaddings(spec);
                    e.digests = digests(spec);
                    e.randomizedEncryptionRequired = randomizedEncryptionRequired(spec);
                } else {
                    e.sigPaddings = new String[] {KeyPropertiesConstants.PADDING_PKCS1,
                            KeyPropertiesConstants.PADDING_NONE};
                    e.digests = new String[] {KeyPropertiesConstants.DIGEST_SHA256,
                            KeyPropertiesConstants.DIGEST_SHA1};
                }
                e.chain = new byte[][] {selfSigned(alias, kp)};
                store(alias, e);
                return new KeyPair(new StorePublicKey(e.publicMaterial, kp.getPublic()),
                        new StorePrivateKey(alias, e.alg));
            } catch (ProviderException ex) {
                throw ex;
            } catch (Throwable t) {
                Throwable cause = t instanceof java.lang.reflect.InvocationTargetException
                        ? t.getCause() : t;
                throw new ProviderException("key pair generation failed: " + cause, cause);
            }
        }
    }

    /**
     * The public half a generated key pair reports. It is the platform's own
     * public key; the wrapper exists so callers see one key type per alias
     * (AOSP returns an {@code AndroidKeyStorePublicKey} here).
     */
    static final class StorePublicKey implements PublicKey {
        private final byte[] encoded;
        private final PublicKey delegate;

        StorePublicKey(byte[] encoded, PublicKey delegate) {
            this.encoded = encoded;
            this.delegate = delegate;
        }

        @Override
        public String getAlgorithm() {
            return delegate.getAlgorithm();
        }

        @Override
        public String getFormat() {
            return delegate.getFormat();
        }

        @Override
        public byte[] getEncoded() {
            return encoded;
        }
    }

    /** {@code KeyPairGenerator} for RSA. */
    public static final class RsaKeyPairGen extends KeyPairGen {
        @Override
        String keyAlgorithm() {
            return "RSA";
        }
    }

    /** {@code KeyPairGenerator} for EC (P-256 by default). */
    public static final class EcKeyPairGen extends KeyPairGen {
        @Override
        String keyAlgorithm() {
            return "EC";
        }
    }

    // ------------------------------------------------------------- KeyInfo SPIs

    /**
     * Build the framework's {@link android.security.keystore.KeyInfo} for an
     * entry. The 22-argument constructor is {@code @hide}; its exact descriptor
     * was read from this image's {@code framework/classes3.dex} and is invoked
     * reflectively, which is how {@code AndroidKeyStoreSecretKeyFactorySpi}
     * builds it inside the framework.
     */
    private static KeySpec keyInfo(String alias, Entry e) {
        try {
            Class<?> c = Class.forName("android.security.keystore.KeyInfo");
            java.lang.reflect.Constructor<?> ctor = c.getDeclaredConstructor(
                    String.class, boolean.class, int.class, int.class,
                    Date.class, Date.class, Date.class, int.class,
                    String[].class, String[].class, String[].class, String[].class,
                    boolean.class, int.class, int.class,
                    boolean.class, boolean.class, boolean.class, boolean.class, boolean.class,
                    int.class, int.class);
            ctor.setAccessible(true);
            return (KeySpec) ctor.newInstance(
                    alias,
                    /* insideSecureHardware */ false,
                    e.origin,
                    e.keySize,
                    e.validStart > 0 ? new Date(e.validStart) : null,
                    e.validEnd > 0 ? new Date(e.validEnd) : null,
                    e.validEnd > 0 ? new Date(e.validEnd) : null,
                    e.purposes,
                    e.encPaddings,
                    e.sigPaddings,
                    e.digests,
                    e.blockModes,
                    /* userAuthenticationRequired */ false,
                    /* userAuthenticationValidityDurationSeconds */ -1,
                    /* userAuthenticationType */ 0,
                    /* enforcedBySecureHardware */ false,
                    /* userAuthenticationValidWhileOnBody */ false,
                    /* trustedUserPresenceRequired */ false,
                    /* invalidatedByBiometricEnrollment */ false,
                    /* userConfirmationRequired */ false,
                    KeyPropertiesConstants.SECURITY_LEVEL_SOFTWARE,
                    /* remainingUsageCount */ -1);
        } catch (Throwable t) {
            Throwable cause = t instanceof java.lang.reflect.InvocationTargetException
                    ? t.getCause() : t;
            throw new ProviderException("ACOS keystore: cannot build KeyInfo: " + cause, cause);
        }
    }

    private static KeySpec keyInfoFor(Key key) throws java.security.spec.InvalidKeySpecException {
        Entry e = entryOf(key);
        if (e == null) {
            throw new java.security.spec.InvalidKeySpecException(
                    "Key is not a key of this AndroidKeyStore");
        }
        return keyInfo(((SoftwareKey) key).alias, e);
    }

    abstract static class InfoFactory extends SecretKeyFactorySpi {
        protected abstract KeySpec engineGetKeySpec(SecretKey key, Class<?> keySpec)
                throws java.security.spec.InvalidKeySpecException;

        @Override
        public SecretKey engineGenerateSecret(KeySpec keySpec)
                throws java.security.spec.InvalidKeySpecException {
            throw new java.security.spec.InvalidKeySpecException(
                    "Key generation via SecretKeyFactory is not supported");
        }

        @Override
        protected SecretKey engineTranslateKey(SecretKey key)
                throws java.security.InvalidKeyException {
            throw new java.security.InvalidKeyException(
                    "Key translation is not supported by this AndroidKeyStore");
        }
    }

    /** {@code SecretKeyFactory} whose only job is {@code KeyInfo}. */
    public static class SecretKeyInfoFactory extends InfoFactory {
        @Override
        protected KeySpec engineGetKeySpec(SecretKey key, Class<?> keySpec)
                throws java.security.spec.InvalidKeySpecException {
            if (keySpec == null) {
                throw new java.security.spec.InvalidKeySpecException("keySpec == null");
            }
            if (!"android.security.keystore.KeyInfo".equals(keySpec.getName())) {
                throw new java.security.spec.InvalidKeySpecException(
                        "Unsupported KeySpec: " + keySpec.getName());
            }
            return keyInfoFor(key);
        }
    }

    /** {@code SecretKeyFactory} for {@code HmacSHA256}. */
    public static final class HmacSHA256InfoFactory extends SecretKeyInfoFactory {
    }

    /** {@code SecretKeyFactory} for {@code HmacSHA1}. */
    public static final class HmacSHA1InfoFactory extends SecretKeyInfoFactory {
    }

    /** {@code KeyFactory} whose only job is {@code KeyInfo}. */
    public abstract static class KeyInfoFactory extends KeyFactorySpi {
        @Override
        protected PublicKey engineGeneratePublic(KeySpec keySpec)
                throws java.security.spec.InvalidKeySpecException {
            throw new java.security.spec.InvalidKeySpecException(
                    "Key generation via KeyFactory is not supported");
        }

        @Override
        protected PrivateKey engineGeneratePrivate(KeySpec keySpec)
                throws java.security.spec.InvalidKeySpecException {
            throw new java.security.spec.InvalidKeySpecException(
                    "Key generation via KeyFactory is not supported");
        }

        @Override
        protected Key engineTranslateKey(Key key) throws java.security.InvalidKeyException {
            throw new java.security.InvalidKeyException(
                    "Key translation is not supported by this AndroidKeyStore");
        }

        @Override
        protected <T extends KeySpec> T engineGetKeySpec(Key key, Class<T> keySpec)
                throws java.security.spec.InvalidKeySpecException {
            if (keySpec == null) {
                throw new java.security.spec.InvalidKeySpecException("keySpec == null");
            }
            if (!"android.security.keystore.KeyInfo".equals(keySpec.getName())) {
                throw new java.security.spec.InvalidKeySpecException(
                        "Unsupported KeySpec: " + keySpec.getName());
            }
            return keySpec.cast(keyInfoFor(key));
        }
    }

    /** {@code KeyFactory} for {@code RSA}. */
    public static final class RsaKeyInfoFactory extends KeyInfoFactory {
    }

    /** {@code KeyFactory} for {@code EC}. */
    public static final class EcKeyInfoFactory extends KeyInfoFactory {
    }

    // ---------------------------------------------------------- guard provider

    /**
     * The per-operation checks AOSP's keystore performs. Installed at provider
     * position 1 so it sees every {@code Cipher}/{@code Mac} initialisation
     * before a functional provider does — the position and purpose of AOSP's
     * {@code AndroidKeyStoreBCWorkaroundProvider}. Keys that did not come from
     * this store are delegated untouched, so a turn of the shim on cannot
     * change how a non-keystore key behaves.
     */
    public static final class GuardProvider extends Provider {
        private static final long serialVersionUID = 1L;

        private static final String[][] CIPHERS = {
            {"AES/CBC/PKCS5Padding", "AES/CBC/PKCS7Padding", "AES/CBC/NoPadding",
                "AES/ECB/PKCS5Padding", "AES/ECB/PKCS7Padding", "AES/ECB/NoPadding",
                "AES/CTR/NoPadding", "AES/GCM/NoPadding"},
            {"RSA/ECB/PKCS1Padding", "RSA/ECB/NoPadding", "RSA/None/PKCS1Padding",
                "RSA/ECB/OAEPWithSHA-256AndMGF1Padding",
                "RSA/ECB/OAEPWithSHA-1AndMGF1Padding",
                "RSA/None/OAEPWithSHA-256AndMGF1Padding",
                "RSA/None/OAEPWithSHA-1AndMGF1Padding"},
        };

        GuardProvider() {
            super("AndroidKeyStoreGuard", 1.0, "ACOS AndroidKeyStore use-time checks");
            for (String[] group : CIPHERS) {
                for (String t : group) {
                    putService(new GuardService(this, "Cipher", t,
                            GuardCipher.class.getName(), null, null));
                }
            }
            putService(new GuardService(this, "Mac", "HmacSHA256",
                    GuardHmacSHA256.class.getName(), null, null));
            putService(new GuardService(this, "Mac", "HmacSHA1",
                    GuardHmacSHA1.class.getName(), null, null));
            // Signature is guarded too: our private keys are opaque (AOSP
            // behaviour), so the platform provider needs the material rebuilt
            // from the store's entry to be able to sign.
            for (String s : SIGNATURES) {
                putService(new GuardService(this, "Signature", s,
                        GuardSignature.class.getName(), null, null));
            }
        }

        private static final String[] SIGNATURES = {
            "SHA256withRSA", "SHA1withRSA", "SHA384withRSA", "SHA512withRSA",
            "SHA256withECDSA", "SHA1withECDSA", "SHA384withECDSA", "SHA512withECDSA",
            "NONEwithRSA", "NONEwithECDSA",
        };
    }

    /**
     * The transformation the SPI was instantiated for. JCA's `CipherSpi`
     * contract does not pass it to the SPI, so the service that created the
     * instance records it for the constructor on the same thread.
     */
    private static final ThreadLocal<String> PENDING_TRANSFORMATION = new ThreadLocal<>();

    /** {@code Provider.Service} that names the transformation to its SPI. */
    static final class GuardService extends Provider.Service {
        GuardService(Provider provider, String type, String algorithm,
                String className, java.util.List<String> aliases,
                java.util.Map<String, String> attributes) {
            super(provider, type, algorithm, className, aliases, attributes);
        }

        /**
         * AOSP's `AndroidKeyStore*Provider` registers its services with
         * `SupportedKeyClasses`, so the platform provider is only offered keys
         * that came from the keystore. Reproduced here: a plain
         * `SecretKeySpec`/Conscrypt key is never routed through the guard (the
         * JCA skips a service whose `supportsParameter` says no), which keeps
         * `Cipher.getProvider()` honest for both paths.
         */
        @Override
        public boolean supportsParameter(Object parameter) {
            if (parameter instanceof Key) return parameter instanceof SoftwareKey;
            return true;
        }

        @Override
        public Object newInstance(Object parameter) throws java.security.NoSuchAlgorithmException {
            PENDING_TRANSFORMATION.set(getAlgorithm());
            try {
                return super.newInstance(parameter);
            } finally {
                PENDING_TRANSFORMATION.remove();
            }
        }
    }

    // ------------------------------------------------ refusals (AOSP shapes)

    /*
     * Refusals use AOSP's own exception types and messages, so an app that
     * catches `InvalidKeyException`/`KeyExpiredException`/
     * `KeyNotYetValidException`/`UserNotAuthenticatedException` behaves as it
     * would on a device. The mapping is `android.security.keystore2.
     * KeyStoreCryptoOperationUtils` (API 34; class read out of this image's
     * `framework/classes3.dex` with `dexdump`):
     *
     *   KM_ERROR_KEY_EXPIRED            -> KeyExpiredException()
     *   KM_ERROR_KEY_NOT_YET_VALID      -> KeyNotYetValidException()
     *   KM_ERROR_CALLER_NONCE_PROHIBITED-> InvalidAlgorithmParameterException(
     *                                       "Caller-provided IV not permitted")
     *   KM_ERROR_INVALID_NONCE          -> InvalidAlgorithmParameterException(
     *                                       "Invalid IV")
     *   otherwise                       -> InvalidKeyException(
     *                                       "Keystore operation failed",
     *                                       KeyStoreException(code, detail))
     *
     * The refusal is *not* swallowed by `javax.crypto.Cipher`'s delayed
     * provider selection any more: the key objects are opaque (above), so a
     * second provider cannot init with them, and `Cipher.tryCombinations`
     * rethrows the FIRST provider's exception (this image's `core-oj`
     * bytecode: the first failure is remembered and rethrown). Measured in
     * `results/compat/K5.md` (K5c).
     */
    private static final int KM_ERROR_INCOMPATIBLE_BLOCK_MODE = -8;
    private static final int KM_ERROR_INCOMPATIBLE_PURPOSE = -3;

    /** Refuse an operation on a key whose characteristics do not allow it. */
    private static void checkUse(Key key, int requiredPurpose) throws InvalidKeyException {
        Entry e = entryOf(key);
        if (e == null) return; // not ours: delegate as before
        String alias = ((SoftwareKey) key).alias;
        long now = System.currentTimeMillis();
        if (e.validStart > 0 && now < e.validStart) {
            throw new android.security.keystore.KeyNotYetValidException();
        }
        if (e.validEnd > 0 && now > e.validEnd) {
            throw new android.security.keystore.KeyExpiredException();
        }
        if ((e.purposes & requiredPurpose) == 0) {
            throw invalidKeyException(KM_ERROR_INCOMPATIBLE_PURPOSE, "key \"" + alias
                    + "\" does not have " + purposeName(requiredPurpose)
                    + " (purposes=0x" + Integer.toHexString(e.purposes) + ")");
        }
    }

    /**
     * AOSP's generic operation refusal: `InvalidKeyException("Keystore
     * operation failed", KeyStoreException(keymasterCode, detail))`.
     */
    private static InvalidKeyException invalidKeyException(int keymasterCode, String detail) {
        return new InvalidKeyException("Keystore operation failed",
                keyStoreException(keymasterCode, detail));
    }

    /**
     * `android.security.KeyStoreException(int, String)` is hidden from the
     * public `android.jar` (its `(int, String)` constructor is flagged BLOCKED),
     * so it is constructed reflectively; the guest has no hidden-API
     * enforcement (verified in `Experiments: K5b`). If the constructor is ever
     * unavailable the cause still identifies the refusal — never silently no
     * exception thrown.
     */
    private static Exception keyStoreException(int code, String detail) {
        try {
            Class<?> cls = Class.forName("android.security.KeyStoreException");
            return (Exception) cls.getConstructor(int.class, String.class)
                    .newInstance(code, detail);
        } catch (Throwable t) {
            return new IllegalStateException("keymaster code " + code + ": " + detail, t);
        }
    }

    private static String purposeName(int purpose) {
        switch (purpose) {
            case KeyPropertiesConstants.PURPOSE_ENCRYPT:
                return "PURPOSE_ENCRYPT";
            case KeyPropertiesConstants.PURPOSE_DECRYPT:
                return "PURPOSE_DECRYPT";
            case KeyPropertiesConstants.PURPOSE_SIGN:
                return "PURPOSE_SIGN";
            case KeyPropertiesConstants.PURPOSE_VERIFY:
                return "PURPOSE_VERIFY";
            case KeyPropertiesConstants.PURPOSE_WRAP_KEY:
                return "PURPOSE_WRAP_KEY";
            case KeyPropertiesConstants.PURPOSE_AGREE_KEY:
                return "PURPOSE_AGREE_KEY";
            default:
                return "purpose 0x" + Integer.toHexString(purpose);
        }
    }

    /** Delegate a transformation to the first provider after this one. */
    private static Cipher delegate(String transformation) throws java.security.NoSuchAlgorithmException {
        for (Provider p : Security.getProviders()) {
            if (p instanceof GuardProvider) continue;
            try {
                return Cipher.getInstance(transformation, p);
            } catch (Throwable ignored) {
                // try the next provider
            }
        }
        throw new java.security.NoSuchAlgorithmException(transformation);
    }

    private static Mac delegateMac(String algorithm) throws java.security.NoSuchAlgorithmException {
        for (Provider p : Security.getProviders()) {
            if (p instanceof GuardProvider) continue;
            try {
                return Mac.getInstance(algorithm, p);
            } catch (Throwable ignored) {
                // try the next provider
            }
        }
        throw new java.security.NoSuchAlgorithmException(algorithm);
    }

    /** {@code Cipher} SPI: enforce this store's key characteristics, then delegate. */
    public static final class GuardCipher extends CipherSpi {
        private Cipher target;
        private String transformation;
        /**
         * The service name JCA picked. `Cipher.getInstance` builds the provider
         * from a mapping whose key is `Cipher.<transformation>`; the SPI itself
         * is only told the transformation through `engineSetMode`/`engineSetPadding`,
         * so it is resolved from the `Service` that instantiated it.
         */

        /** JCA instantiates an SPI through its public no-arg constructor. */
        public GuardCipher() {
            this.transformation = PENDING_TRANSFORMATION.get();
        }

        private void prepare(int opmode, Key key, AlgorithmParameterSpec params,
                SecureRandom random) throws InvalidKeyException,
                InvalidAlgorithmParameterException {
            String t = transformation;
            if (t == null) {
                throw new InvalidKeyException("AndroidKeyStoreGuard: unknown transformation");
            }
            int purpose = opmode == Cipher.ENCRYPT_MODE
                    ? KeyPropertiesConstants.PURPOSE_ENCRYPT
                    : KeyPropertiesConstants.PURPOSE_DECRYPT;
            checkUse(key, purpose);
            Entry e = entryOf(key);
            if (e != null && opmode == Cipher.ENCRYPT_MODE
                    && e.randomizedEncryptionRequired
                    && (params instanceof IvParameterSpec
                            || params instanceof javax.crypto.spec.GCMParameterSpec)) {
                // AOSP: KM_ERROR_CALLER_NONCE_PROHIBITED -> this exact message.
                throw new InvalidAlgorithmParameterException(
                        "Caller-provided IV not permitted");
            }
            if (e != null) {
                String mode = modeOf(t);
                if (mode != null && e.blockModes.length > 0 && !contains(e.blockModes, mode)) {
                    throw invalidKeyException(KM_ERROR_INCOMPATIBLE_BLOCK_MODE, mode
                            + " is not permitted for key \"" + ((SoftwareKey) key).alias + "\"");
                }
            }
            // The key handed to the delegate is built from THIS store's entry,
            // never from the (opaque) key object the caller passed in.
            Key material = e == null ? key
                    : ("secret".equals(e.type) ? materialSecret(key) : materialPrivate(key));
            if (material == null) material = key;
            try {
                target = delegate(t);
                if (params == null) {
                    target.init(opmode, material, random);
                } else {
                    target.init(opmode, material, params, random);
                }
            } catch (InvalidKeyException | InvalidAlgorithmParameterException ex) {
                throw ex;
            } catch (Throwable ex) {
                throw new InvalidKeyException("AndroidKeyStoreGuard: " + t + ": " + ex, ex);
            }
        }

        private static String modeOf(String transformation) {
            int i = transformation.indexOf('/');
            if (i < 0) return null;
            int j = transformation.indexOf('/', i + 1);
            return j < 0 ? transformation.substring(i + 1) : transformation.substring(i + 1, j);
        }

        private static boolean contains(String[] values, String want) {
            for (String v : values) {
                if (v != null && v.equalsIgnoreCase(want)) return true;
            }
            return false;
        }

        @Override
        protected void engineSetMode(String mode) {
            transformation = "AES/" + mode;
        }

        @Override
        protected void engineSetPadding(String padding) {
            transformation = "AES/" + padding;
        }

        @Override
        protected void engineInit(int opmode, Key key, SecureRandom random)
                throws InvalidKeyException {
            try {
                prepare(opmode, key, null, random);
            } catch (InvalidAlgorithmParameterException e) {
                throw new InvalidKeyException(e.getMessage(), e);
            }
        }

        @Override
        protected void engineInit(int opmode, Key key, AlgorithmParameterSpec params,
                SecureRandom random) throws InvalidKeyException, InvalidAlgorithmParameterException {
            prepare(opmode, key, params, random);
        }

        @Override
        protected void engineInit(int opmode, Key key, AlgorithmParameters params,
                SecureRandom random) throws InvalidKeyException, InvalidAlgorithmParameterException {
            prepare(opmode, key, params == null ? null : specOf(params), random);
        }

        private static AlgorithmParameterSpec specOf(AlgorithmParameters params) {
            try {
                return params.getParameterSpec(IvParameterSpec.class);
            } catch (Throwable t) {
                return null;
            }
        }

        @Override
        protected byte[] engineUpdate(byte[] input, int offset, int len) {
            return target.update(input, offset, len);
        }

        @Override
        protected int engineUpdate(byte[] input, int offset, int len, byte[] output, int outOff)
                throws javax.crypto.ShortBufferException {
            return target.update(input, offset, len, output, outOff);
        }

        @Override
        protected byte[] engineDoFinal(byte[] input, int offset, int len)
                throws javax.crypto.IllegalBlockSizeException, javax.crypto.BadPaddingException {
            return target.doFinal(input, offset, len);
        }

        @Override
        protected int engineDoFinal(byte[] input, int offset, int len, byte[] output, int outOff)
                throws javax.crypto.ShortBufferException, javax.crypto.IllegalBlockSizeException,
                javax.crypto.BadPaddingException {
            return target.doFinal(input, offset, len, output, outOff);
        }

        @Override
        protected int engineGetBlockSize() {
            return target == null ? 16 : target.getBlockSize();
        }

        @Override
        protected byte[] engineGetIV() {
            return target == null ? null : target.getIV();
        }

        @Override
        protected int engineGetOutputSize(int inputLen) {
            return target == null ? inputLen + 16 : target.getOutputSize(inputLen);
        }

        @Override
        protected AlgorithmParameters engineGetParameters() {
            return target == null ? null : target.getParameters();
        }
    }

    /**
     * {@code Signature} SPI: enforce SIGN purpose/validity, then delegate with
     * the material rebuilt from the store (the caller's key is opaque).
     */
    public static final class GuardSignature extends java.security.SignatureSpi {
        private final String algorithm;
        private Signature target;

        /** JCA instantiates an SPI through its public no-arg constructor. */
        public GuardSignature() {
            this.algorithm = PENDING_TRANSFORMATION.get();
        }

        private Signature target() throws java.security.NoSuchAlgorithmException {
            if (target == null) {
                String alg = algorithm;
                if (alg == null) {
                    throw new java.security.NoSuchAlgorithmException("AndroidKeyStoreGuard");
                }
                for (Provider p : Security.getProviders()) {
                    if (p instanceof GuardProvider) continue;
                    try {
                        target = Signature.getInstance(alg, p);
                        return target;
                    } catch (Throwable ignoredEx) {
                        // next provider
                    }
                }
                throw new java.security.NoSuchAlgorithmException(alg);
            }
            return target;
        }

        @Override
        protected void engineInitVerify(PublicKey publicKey) throws InvalidKeyException {
            try {
                target().initVerify(publicKey);
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new InvalidKeyException("Keystore operation failed", e);
            }
        }

        @Override
        protected void engineInitSign(PrivateKey privateKey) throws InvalidKeyException {
            engineInitSign(privateKey, null);
        }

        @Override
        protected void engineInitSign(PrivateKey privateKey, SecureRandom random)
                throws InvalidKeyException {
            checkUse(privateKey, KeyPropertiesConstants.PURPOSE_SIGN);
            PrivateKey material = entryOf(privateKey) == null
                    ? privateKey : materialPrivate(privateKey);
            try {
                if (random == null) {
                    target().initSign(material == null ? privateKey : material);
                } else {
                    target().initSign(material == null ? privateKey : material, random);
                }
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new InvalidKeyException("Keystore operation failed", e);
            }
        }

        @Override
        protected void engineUpdate(byte b) throws java.security.SignatureException {
            try {
                target().update(b);
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new java.security.SignatureException(e.toString());
            }
        }

        @Override
        protected void engineUpdate(byte[] b, int off, int len)
                throws java.security.SignatureException {
            try {
                target().update(b, off, len);
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new java.security.SignatureException(e.toString());
            }
        }

        @Override
        protected byte[] engineSign() throws java.security.SignatureException {
            try {
                return target().sign();
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new java.security.SignatureException(e.toString());
            }
        }

        @Override
        protected boolean engineVerify(byte[] sigBytes) throws java.security.SignatureException {
            try {
                return target().verify(sigBytes);
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new java.security.SignatureException(e.toString());
            }
        }

        @Override
        protected void engineSetParameter(String param, Object value)
                throws java.security.InvalidParameterException {
            throw new java.security.InvalidParameterException(param);
        }

        @Override
        protected Object engineGetParameter(String param)
                throws java.security.InvalidParameterException {
            throw new java.security.InvalidParameterException(param);
        }
    }

    /** {@code Mac} SPI: same purpose/validity checks for HMAC signing keys. */
    public abstract static class GuardMac extends MacSpi {
        private final Mac target;

        GuardMac(String algorithm) throws java.security.NoSuchAlgorithmException {
            this.target = delegateMac(algorithm);
        }

        @Override
        protected int engineGetMacLength() {
            return target.getMacLength();
        }

        @Override
        protected void engineInit(Key key, AlgorithmParameterSpec params)
                throws InvalidKeyException, InvalidAlgorithmParameterException {
            checkUse(key, KeyPropertiesConstants.PURPOSE_SIGN);
            Key material = entryOf(key) == null ? key : materialSecret(key);
            target.init(material == null ? key : material, params);
        }

        @Override
        protected void engineUpdate(byte input) {
            target.update(input);
        }

        @Override
        protected void engineUpdate(byte[] input, int offset, int len) {
            target.update(input, offset, len);
        }

        @Override
        protected byte[] engineDoFinal() {
            return target.doFinal();
        }

        @Override
        protected void engineReset() {
            target.reset();
        }
    }

    /** {@code Mac.HmacSHA256} under the guard. */
    public static final class GuardHmacSHA256 extends GuardMac {
        public GuardHmacSHA256() throws java.security.NoSuchAlgorithmException {
            super("HmacSHA256");
        }
    }

    /** {@code Mac.HmacSHA1} under the guard. */
    public static final class GuardHmacSHA1 extends GuardMac {
        public GuardHmacSHA1() throws java.security.NoSuchAlgorithmException {
            super("HmacSHA1");
        }
    }

    // ------------------------------------------------------- certificate chain

    /**
     * A self-signed X.509 v3 certificate for a generated key pair, DER-encoded
     * and signed with the pair's own private key: the chain shape
     * {@code AndroidKeyStoreSpi} reports for generated keys (one certificate,
     * subject == issuer).
     *
     * <p>DER is built here because this image ships Conscrypt and no
     * BouncyCastle certificate builder (checked: no
     * {@code com/android/org/bouncycastle} in the boot dexes).
     */
    private static byte[] selfSigned(String alias, KeyPair kp) throws Exception {
        boolean ec = "EC".equals(kp.getPrivate().getAlgorithm());
        String sigAlg = ec ? "SHA256withECDSA" : "SHA256withRSA";
        byte[] subject = derName(alias);
        byte[] algorithm = ec ? derAlgorithm("1.2.840.10045.4.3.2", null)
                : derAlgorithm("1.2.840.113549.1.1.11", new byte[] {0x05, 0x00});
        long now = System.currentTimeMillis();
        byte[] validity = derSequence(concat(derUtcTime(new Date(now - 86400000L)),
                derUtcTime(new Date(now + 30L * 365L * 86400000L))));
        byte[] tbs = derSequence(concat(
                derExplicit(0, derInteger(BigInteger.valueOf(2))),
                derInteger(BigInteger.valueOf(System.currentTimeMillis())),
                algorithm,
                subject,
                validity,
                subject,
                derRaw(kp.getPublic().getEncoded())));
        Signature signer = Signature.getInstance(sigAlg);
        signer.initSign(kp.getPrivate());
        signer.update(tbs);
        byte[] signature = signer.sign();
        return derSequence(concat(tbs, algorithm, derRaw(derBitString(signature))));
    }

    private static byte[] derName(String commonName) {
        byte[] cn = derSequence(concat(
                derOid("2.5.4.3"),
                derUtf8(commonName)));
        return derSequence(new byte[][] {derSet(cn)});
    }

    private static byte[] derAlgorithm(String oid, byte[] params) {
        return params == null ? derSequence(new byte[][] {derOid(oid)})
                : derSequence(concat(derOid(oid), params));
    }

    private static byte[] derOid(String oid) {
        String[] parts = oid.split("\\.");
        ByteArrayOutputStream body = new ByteArrayOutputStream();
        body.write(Integer.parseInt(parts[0]) * 40 + Integer.parseInt(parts[1]));
        for (int i = 2; i < parts.length; i++) {
            long v = Long.parseLong(parts[i]);
            int shift = 0;
            long tmp = v;
            while (tmp > 0x7f) {
                tmp >>= 7;
                shift += 7;
            }
            for (int s = shift; s > 0; s -= 7) {
                body.write((int) (((v >> s) & 0x7f) | 0x80));
            }
            body.write((int) (v & 0x7f));
        }
        return derTagged(0x06, body.toByteArray());
    }

    private static byte[] derInteger(BigInteger v) {
        return derTagged(0x02, v.toByteArray());
    }

    private static byte[] derUtf8(String s) {
        try {
            return derTagged(0x0c, s.getBytes("UTF-8"));
        } catch (java.io.UnsupportedEncodingException e) {
            throw new IllegalStateException(e);
        }
    }

    private static byte[] derUtcTime(Date d) {
        java.text.SimpleDateFormat f = new java.text.SimpleDateFormat("yyMMddHHmmss'Z'");
        f.setTimeZone(java.util.TimeZone.getTimeZone("UTC"));
        try {
            return derTagged(0x17, f.format(d).getBytes("US-ASCII"));
        } catch (java.io.UnsupportedEncodingException e) {
            throw new IllegalStateException(e);
        }
    }

    private static byte[] derBitString(byte[] bytes) {
        byte[] body = new byte[bytes.length + 1];
        System.arraycopy(bytes, 0, body, 1, bytes.length);
        return derTagged(0x03, body);
    }

    private static byte[] derExplicit(int tag, byte[] inner) {
        return derTagged(0xa0 + tag, inner);
    }

    private static byte[] derSequence(byte[] body) {
        return derTagged(0x30, body);
    }

    private static byte[] derSequence(byte[][] items) {
        return derTagged(0x30, concat(items));
    }

    private static byte[] derSet(byte[] body) {
        return derTagged(0x31, body);
    }

    private static byte[] derRaw(byte[] encoded) {
        return encoded;
    }

    private static byte[] derTagged(int tag, byte[] body) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        out.write(tag);
        int len = body.length;
        if (len < 0x80) {
            out.write(len);
        } else if (len < 0x100) {
            out.write(0x81);
            out.write(len);
        } else if (len < 0x10000) {
            out.write(0x82);
            out.write(len >> 8);
            out.write(len & 0xff);
        } else {
            out.write(0x83);
            out.write(len >> 16);
            out.write((len >> 8) & 0xff);
            out.write(len & 0xff);
        }
        out.write(body, 0, body.length);
        return out.toByteArray();
    }

    private static byte[] concat(byte[]... parts) {
        int n = 0;
        for (byte[] p : parts) n += p.length;
        byte[] out = new byte[n];
        int off = 0;
        for (byte[] p : parts) {
            System.arraycopy(p, 0, out, off, p.length);
            off += p.length;
        }
        return out;
    }
}
