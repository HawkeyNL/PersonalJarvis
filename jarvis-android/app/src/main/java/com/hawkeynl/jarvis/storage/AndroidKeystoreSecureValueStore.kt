package com.hawkeynl.jarvis.storage

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.nio.ByteBuffer
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Stores only AES-GCM ciphertext in app-private preferences. The non-exportable
 * wrapping key lives in Android Keystore and is usable only while the device is unlocked.
 */
class AndroidKeystoreSecureValueStore(context: Context) : SecureValueStore {
    private val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    @Synchronized
    override fun read(key: String): ByteArray? {
        val encoded = preferences.getString(key, null) ?: return null
        return try {
            val blob = Base64.decode(encoded, Base64.NO_WRAP)
            require(blob.size > IV_BYTES)
            val buffer = ByteBuffer.wrap(blob)
            val ivLength = buffer.get().toInt() and 0xff
            require(ivLength in 12..16 && blob.size > ivLength + 1)
            val iv = ByteArray(ivLength).also { buffer.get(it) }
            val ciphertext = ByteArray(buffer.remaining()).also { buffer.get(it) }
            Cipher.getInstance(TRANSFORMATION).run {
                init(Cipher.DECRYPT_MODE, wrappingKey(), GCMParameterSpec(TAG_BITS, iv))
                updateAAD(key.toByteArray(Charsets.UTF_8))
                doFinal(ciphertext)
            }
        } catch (error: Exception) {
            throw SecureStorageException("Beveiligde opslag kan niet worden ontsleuteld.", error)
        }
    }

    @Synchronized
    override fun write(key: String, value: ByteArray) {
        try {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, wrappingKey())
            cipher.updateAAD(key.toByteArray(Charsets.UTF_8))
            val ciphertext = cipher.doFinal(value)
            val blob = ByteBuffer.allocate(1 + cipher.iv.size + ciphertext.size)
                .put(cipher.iv.size.toByte())
                .put(cipher.iv)
                .put(ciphertext)
                .array()
            check(preferences.edit().putString(key, Base64.encodeToString(blob, Base64.NO_WRAP)).commit())
        } catch (error: Exception) {
            throw SecureStorageException("Beveiligde opslag kan niet worden bijgewerkt.", error)
        }
    }

    @Synchronized
    override fun remove(key: String) {
        if (!preferences.edit().remove(key).commit()) {
            throw SecureStorageException("Beveiligde opslag kan niet worden gewist.")
        }
    }

    override fun exists(key: String): Boolean = preferences.contains(key)

    private fun wrappingKey(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUnlockedDeviceRequired(true)
            .build()
        generator.init(spec)
        return generator.generateKey()
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val KEY_ALIAS = "com.hawkeynl.jarvis.secrets.v1"
        const val PREFERENCES = "jarvis_secure_ciphertext"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val TAG_BITS = 128
        const val IV_BYTES = 12
    }
}
