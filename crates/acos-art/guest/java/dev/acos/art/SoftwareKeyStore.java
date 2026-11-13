package dev.acos.art;

import android.security.keystore.KeyGenParameterSpec;
import android.util.Base64;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.AlgorithmParameters;
import java.security.InvalidAlgorithmParameterException;
import java.security.InvalidParameterException;
import java.security.Key;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyPairGeneratorSpi;
import java.security.KeyStore;
import java.security.KeyStoreSpi;
import java.security.Provider;
import java.security.ProviderException;
import java.security.SecureRandom;
import java.security.Security;
import java.security.cert.Certificate;
import java.security.cert.CertificateFactory;
import java.security.spec.AlgorithmParameterSpec;
import java.security.spec.ECGenParameterSpec;
import java.security.spec.PKCS8EncodedKeySpec;
import java.security.spec.X509EncodedKeySpec;
import java.util.Collections;
import java.util.Enumeration;
import java.util.LinkedHashMap;
import java.util.Properties;
import java.util.Vector;
import javax.crypto.KeyGeneratorSpi;
import javax.crypto.SecretKey;
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
 *   <li>keys are ordinary JCA {@link SecretKeySpec}/{@link java.security.PrivateKey}
 *       objects (software, exportable inside the process) — nothing claims
 *       {@code KeyInfo} hardware backing, TEE, StrongBox or attestation;</li>
 *   <li>a spec that requires user authentication
 *       ({@code setUserAuthenticationRequired(true)}) or StrongBox is
 *       <b>rejected</b> with {@link InvalidAlgorithmParameterException} rather
 *       than silently generating a key usable without that protection;</li>
 *   <li>every session gets a fresh, private directory (the run root is deleted
 *       when the session ends), so keys never leave the sandbox and are never
 *       shared between sessions or apps.</li>
 * </ul>
 *
 * <p>It implements the services the corpus actually asks for: the
 * {@code AndroidKeyStore} {@link KeyStore}, AES/HMAC {@link
 * javax.crypto.KeyGenerator}s (the {@code KeyGenParameterSpec} path Tink and
 * element's {@code KeyStoreSecretKeyRepository} use) and RSA/EC {@link
 * KeyPairGenerator}s, plus the alias persistence a real keystore gives:
 * {@code generateKey()}/{@code generateKeyPair()} create an entry that a later
 * {@code getKey}/{@code getEntry} returns.
 */
public final class SoftwareKeyStore extends Provider {
    private static final long serialVersionUID = 1L;

    static final String NAME = "AndroidKeyStore";
    /** Set by {@link #install(String)}; the session's private storage. */
    private static File sDir;
    private static boolean sInstalled;

    private SoftwareKeyStore() {
        super(NAME, 1.0, "ACOS session-private software keystore");
        put("KeyStore." + NAME, Store.class.getName());
        put("KeyGenerator.AES", AesKeyGen.class.getName());
        put("KeyGenerator.HmacSHA256", HmacSHA256KeyGen.class.getName());
        put("KeyGenerator.HmacSHA1", HmacSHA1KeyGen.class.getName());
        put("KeyPairGenerator.RSA", RsaKeyPairGen.class.getName());
        put("KeyPairGenerator.EC", EcKeyPairGen.class.getName());
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
        //noinspection ResultOfMethodCallIgnored
        dir.mkdirs();
        sDir = dir;
        if (Security.getProvider(NAME) == null) {
            Security.addProvider(new SoftwareKeyStore());
            System.out.println("ACOSLAUNCH MARK keystore provider=software dir=" + dir);
        } else {
            System.out.println("ACOSLAUNCH MARK keystore provider=present dir=" + dir);
        }
        sInstalled = true;
    }

    private static synchronized File dir() {
        if (sDir == null) {
            // Defensive: install() normally ran before any app code. A provider
            // without a directory would silently lose keys, so keep it inside
            // the session's private data root.
            sDir = new File("/data/app-data/acos-keystore");
            //noinspection ResultOfMethodCallIgnored
            sDir.mkdirs();
        }
        return sDir;
    }

    /** One stored alias: a secret key, a key pair or a certificate. */
    private static final class Entry {
        String type; // "secret" | "private" | "certificate"
        String alg = "";
        byte[] material = new byte[0];
        byte[] publicMaterial = new byte[0];
        byte[][] chain = new byte[0][];
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
            System.out.println("ACOSLAUNCH WARN keystore load: " + t);
            return;
        }
        for (String k : p.stringPropertyNames()) {
            if (!k.endsWith(".type")) continue;
            String alias = k.substring(0, k.length() - ".type".length());
            Entry e = new Entry();
            e.type = p.getProperty(k);
            e.alg = p.getProperty(alias + ".alg", "");
            e.material = decode(p.getProperty(alias + ".material"));
            e.publicMaterial = decode(p.getProperty(alias + ".public"));
            String chain = p.getProperty(alias + ".chain", "");
            if (chain.length() > 0) {
                String[] parts = chain.split(",");
                e.chain = new byte[parts.length][];
                for (int i = 0; i < parts.length; i++) e.chain[i] = decode(parts[i]);
            }
            ENTRIES.put(alias, e);
        }
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

    private static synchronized void persist() {
        Properties p = new Properties();
        for (java.util.Map.Entry<String, Entry> me : ENTRIES.entrySet()) {
            String alias = me.getKey();
            Entry e = me.getValue();
            p.setProperty(alias + ".type", e.type);
            p.setProperty(alias + ".alg", e.alg);
            p.setProperty(alias + ".material", encode(e.material));
            p.setProperty(alias + ".public", encode(e.publicMaterial));
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
            System.out.println("ACOSLAUNCH WARN keystore store: " + t);
            return;
        }
        if (!tmp.renameTo(f)) {
            //noinspection ResultOfMethodCallIgnored
            f.delete();
            //noinspection ResultOfMethodCallIgnored
            tmp.renameTo(f);
        }
    }

    private static SecretKey asSecret(Entry e) {
        if (e == null || !"secret".equals(e.type)) return null;
        String alg = e.alg.length() > 0 ? e.alg : "AES";
        return new SecretKeySpec(e.material, alg);
    }

    private static Key asPrivate(Entry e) throws Exception {
        if (e == null || !"private".equals(e.type)) return null;
        KeyFactory kf = KeyFactory.getInstance(e.alg.length() > 0 ? e.alg : "RSA");
        return kf.generatePrivate(new PKCS8EncodedKeySpec(e.material));
    }

    private static java.security.PublicKey asPublic(Entry e) throws Exception {
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

    /** The services-level checks every generator shares. */
    private static void checkSpec(KeyGenParameterSpec spec)
            throws InvalidAlgorithmParameterException {
        if (spec.getKeystoreAlias() == null || spec.getKeystoreAlias().isEmpty()) {
            throw new InvalidAlgorithmParameterException("AndroidKeyStore needs an alias");
        }
        if (spec.isUserAuthenticationRequired()) {
            // A software keystore cannot enforce "unlock with the user"; a key
            // that ignores that requirement would be a security lie.
            throw new InvalidAlgorithmParameterException(
                    "ACOS software keystore cannot enforce user authentication");
        }
        if (spec.isStrongBoxBacked()) {
            throw new InvalidAlgorithmParameterException(
                    "ACOS software keystore has no StrongBox");
        }
    }

    private static void storeSecret(String alias, String alg, SecretKey key) {
        Entry e = new Entry();
        e.type = "secret";
        e.alg = alg;
        e.material = key.getEncoded();
        ENTRIES.put(alias, e);
        persist();
    }

    /** {@code KeyStoreSpi} over the session-private property file. */
    public static final class Store extends KeyStoreSpi {
        @Override
        public Key engineGetKey(String alias, char[] password) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = ENTRIES.get(alias);
                if (e == null) return null;
                try {
                    if ("secret".equals(e.type)) return asSecret(e);
                    if ("private".equals(e.type)) return asPrivate(e);
                } catch (Throwable t) {
                    return null;
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
        public java.util.Date engineGetCreationDate(String alias) {
            return null;
        }

        @Override
        public void engineSetKeyEntry(String alias, Key key, char[] password, Certificate[] chain)
                throws java.security.KeyStoreException {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                if (key == null) throw new java.security.KeyStoreException("null key");
                Entry e = new Entry();
                if (key instanceof SecretKey) {
                    e.type = "secret";
                    e.alg = key.getAlgorithm();
                    e.material = key.getEncoded();
                } else if (key instanceof java.security.PrivateKey) {
                    e.type = "private";
                    e.alg = key.getAlgorithm();
                    e.material = key.getEncoded();
                    if (chain != null) {
                        e.chain = new byte[chain.length][];
                        for (int i = 0; i < chain.length; i++) {
                            try {
                                e.chain[i] = chain[i].getEncoded();
                            } catch (Throwable t) {
                                e.chain[i] = new byte[0];
                            }
                        }
                    }
                } else {
                    throw new java.security.KeyStoreException("unsupported key " + key.getClass());
                }
                ENTRIES.put(alias, e);
                persist();
            }
        }

        @Override
        public void engineSetKeyEntry(String alias, byte[] key, Certificate[] chain)
                throws java.security.KeyStoreException {
            throw new java.security.KeyStoreException("encoded keys are not supported");
        }

        @Override
        public void engineSetCertificateEntry(String alias, Certificate cert)
                throws java.security.KeyStoreException {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                Entry e = new Entry();
                e.type = "certificate";
                try {
                    e.material = cert.getEncoded();
                } catch (Throwable t) {
                    throw new java.security.KeyStoreException("certificate encoding failed");
                }
                ENTRIES.put(alias, e);
                persist();
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
                        return new KeyStore.SecretKeyEntry(asSecret(e));
                    }
                    if ("private".equals(e.type)) {
                        Certificate[] chain = asChain(e);
                        return new KeyStore.PrivateKeyEntry(
                                (java.security.PrivateKey) asPrivate(e),
                                chain != null ? chain : new Certificate[0]);
                    }
                } catch (Throwable t) {
                    return null;
                }
                return null;
            }
        }

        @Override
        public void engineSetEntry(String alias, KeyStore.Entry entry,
                KeyStore.ProtectionParameter protParam) throws java.security.KeyStoreException {
            if (entry instanceof KeyStore.SecretKeyEntry) {
                SecretKey k = ((KeyStore.SecretKeyEntry) entry).getSecretKey();
                engineSetKeyEntry(alias, k, null, null);
            } else if (entry instanceof KeyStore.PrivateKeyEntry) {
                KeyStore.PrivateKeyEntry p = (KeyStore.PrivateKeyEntry) entry;
                engineSetKeyEntry(alias, p.getPrivateKey(), null, p.getCertificateChain());
            } else {
                throw new java.security.KeyStoreException("unsupported entry " + entry.getClass());
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
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                persist();
            }
        }

        @Override
        public void engineStore(KeyStore.LoadStoreParameter param) {
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                persist();
            }
        }
    }

    private abstract static class Gen extends KeyGeneratorSpi {
        private String alias;
        private int keySize;
        private SecureRandom random = new SecureRandom();

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
            KeyGenParameterSpec spec = (KeyGenParameterSpec) params;
            checkSpec(spec);
            alias = spec.getKeystoreAlias();
            if (spec.getKeySize() > 0) keySize = spec.getKeySize();
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
            SecretKeySpec key = new SecretKeySpec(material, jcaAlgorithm());
            synchronized (SoftwareKeyStore.class) {
                ensureLoaded();
                if (alias != null) storeSecret(alias, jcaAlgorithm(), key);
            }
            return key;
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
     * platform's default (software) JCA provider and the private half is kept
     * under the alias, which is what the AndroidKeyStore API guarantees.
     */
    public abstract static class KeyPairGen extends KeyPairGeneratorSpi {
        /** The JCA key algorithm (`RSA`/`EC`); each service has its own class. */
        abstract String keyAlgorithm();

        private String alias;
        private int keySize;
        private ECGenParameterSpec ecParams;
        private SecureRandom random = new SecureRandom();

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
            KeyGenParameterSpec spec = (KeyGenParameterSpec) params;
            checkSpec(spec);
            alias = spec.getKeystoreAlias();
            if (spec.getKeySize() > 0) keySize = spec.getKeySize();
            AlgorithmParameterSpec inner = spec.getAlgorithmParameterSpec();
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
                synchronized (SoftwareKeyStore.class) {
                    ensureLoaded();
                    if (alias != null) {
                        Entry e = new Entry();
                        e.type = "private";
                        e.alg = kp.getPrivate().getAlgorithm();
                        e.material = kp.getPrivate().getEncoded();
                        e.publicMaterial = kp.getPublic().getEncoded();
                        ENTRIES.put(alias, e);
                        persist();
                    }
                }
                return kp;
            } catch (java.security.GeneralSecurityException e) {
                throw new ProviderException("key pair generation failed", e);
            }
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
}
