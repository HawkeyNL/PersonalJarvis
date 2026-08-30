package com.hawkeynl.jarvis.update

import com.hawkeynl.jarvis.network.AndroidUpdateArtifact
import com.hawkeynl.jarvis.network.AndroidUpdateMetadata
import com.hawkeynl.jarvis.network.EndpointValidation
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidUpdatePolicyTest {
    private val endpoint = (HomeNodeEndpoint.parse("https://jarvis.example:8443") as EndpointValidation.Valid).endpoint
    private val signer = "a".repeat(64)

    @Test
    fun `no metadata means current version`() {
        assertEquals(AndroidUpdateDecision.Current, AndroidUpdatePolicy.evaluate(endpoint, 7, signer, null))
    }

    @Test
    fun `newer valid release is available`() {
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, metadata()) is AndroidUpdateDecision.Available)
    }

    @Test
    fun `stale version code is rejected instead of offered`() {
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 8, signer, metadata(versionCode = 8)) is AndroidUpdateDecision.Invalid)
    }

    @Test
    fun `different origin and port are rejected before authenticated download`() {
        val otherHost = metadata(downloadUrl = "https://mirror.example:8443/v1/app-updates/android/download")
        val otherPort = metadata(downloadUrl = "https://jarvis.example:9443/v1/app-updates/android/download")
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, otherHost) is AndroidUpdateDecision.Invalid)
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, otherPort) is AndroidUpdateDecision.Invalid)
    }

    @Test
    fun `origin comparison uses effective port and rejects credentials`() {
        val defaultPortEndpoint = (HomeNodeEndpoint.parse("https://jarvis.example") as EndpointValidation.Valid).endpoint
        val explicitDefaultPort = metadata(
            downloadUrl = "https://jarvis.example:443/v1/app-updates/android/download",
        )
        val credentials = metadata(
            downloadUrl = "https://token@jarvis.example:8443/v1/app-updates/android/download",
        )
        assertTrue(
            AndroidUpdatePolicy.evaluate(defaultPortEndpoint, 7, signer, explicitDefaultPort) is
                AndroidUpdateDecision.Available,
        )
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, credentials) is AndroidUpdateDecision.Invalid)
    }

    @Test
    fun `incompatible protocol and wrong signer are rejected`() {
        val incompatible = metadata(minimumProtocol = ANDROID_CLIENT_PROTOCOL + 1)
        val wrongSigner = metadata(signer = "b".repeat(64))
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, incompatible) is AndroidUpdateDecision.Invalid)
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, wrongSigner) is AndroidUpdateDecision.Invalid)
    }

    @Test
    fun `malformed hash package and version are rejected`() {
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, metadata(hash = "bad")) is AndroidUpdateDecision.Invalid)
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, metadata(packageName = "evil.app")) is AndroidUpdateDecision.Invalid)
        assertTrue(AndroidUpdatePolicy.evaluate(endpoint, 7, signer, metadata(versionName = "latest")) is AndroidUpdateDecision.Invalid)
    }

    private fun metadata(
        versionCode: Int = 8,
        versionName: String = "1.2.3",
        minimumProtocol: Int = ANDROID_CLIENT_PROTOCOL,
        packageName: String = JARVIS_ANDROID_PACKAGE,
        hash: String = "b".repeat(64),
        signer: String = this.signer,
        downloadUrl: String = "${endpoint.baseUrl}/v1/app-updates/android/download",
    ) = AndroidUpdateMetadata(
        schema_version = 1,
        platform = "android",
        package_name = packageName,
        version_code = versionCode,
        version_name = versionName,
        minimum_client_protocol = minimumProtocol,
        released_at = "2026-08-30T12:00:00Z",
        notes = "Update",
        artifact = AndroidUpdateArtifact(1024, hash, signer),
        download_url = downloadUrl,
    )
}
