package com.hawkeynl.jarvis.security

import com.hawkeynl.jarvis.storage.SecureValueStore
import java.security.SecureRandom
import org.bouncycastle.crypto.params.Ed25519PrivateKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer

/**
 * Android Keystore does not offer Ed25519 signing on all supported Android releases.
 * A unique 32-byte Ed25519 seed is therefore generated in native code and persisted
 * only as ciphertext protected by the non-exportable Android Keystore AES key.
 */
class KeystoreBackedEd25519Identity(
    private val secureStore: SecureValueStore,
    private val random: SecureRandom = SecureRandom(),
) : DeviceIdentity {
    @Synchronized
    override fun publicKeyHex(): String = withPrivateKey { key ->
        Hex.encode(key.generatePublicKey().encoded)
    }

    @Synchronized
    override fun signHex(messageHex: String): String {
        val message = Hex.decode(messageHex)
        return withPrivateKey { key ->
            val signer = Ed25519Signer()
            signer.init(true, key)
            signer.update(message, 0, message.size)
            Hex.encode(signer.generateSignature())
        }
    }

    @Synchronized
    override fun reset() = secureStore.remove(SEED_KEY)

    private fun <T> withPrivateKey(block: (Ed25519PrivateKeyParameters) -> T): T {
        val seed = secureStore.read(SEED_KEY) ?: ByteArray(Ed25519PrivateKeyParameters.KEY_SIZE).also {
            random.nextBytes(it)
            secureStore.write(SEED_KEY, it)
        }
        return try {
            block(Ed25519PrivateKeyParameters(seed, 0))
        } finally {
            seed.fill(0)
        }
    }

    private companion object {
        const val SEED_KEY = "device-ed25519-seed.v1"
    }
}
