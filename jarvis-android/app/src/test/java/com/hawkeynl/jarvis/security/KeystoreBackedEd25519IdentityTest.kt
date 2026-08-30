package com.hawkeynl.jarvis.security

import com.hawkeynl.jarvis.testing.InMemorySecureValueStore
import org.bouncycastle.crypto.params.Ed25519PublicKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class KeystoreBackedEd25519IdentityTest {
    @Test
    fun `creates stable raw key and valid signature without exposing private key`() {
        val secureValues = InMemorySecureValueStore()
        val identity = KeystoreBackedEd25519Identity(secureValues)
        val publicKey = identity.publicKeyHex()
        val message = ByteArray(32) { it.toByte() }
        val signature = Hex.decode(identity.signHex(Hex.encode(message)))

        assertEquals(64, publicKey.length)
        assertEquals(publicKey, identity.publicKeyHex())
        assertEquals(64, signature.size)

        val verifier = Ed25519Signer().apply {
            init(false, Ed25519PublicKeyParameters(Hex.decode(publicKey), 0))
            update(message, 0, message.size)
        }
        assertTrue(verifier.verifySignature(signature))

        identity.reset()
        assertFalse(publicKey == identity.publicKeyHex())
    }
}
