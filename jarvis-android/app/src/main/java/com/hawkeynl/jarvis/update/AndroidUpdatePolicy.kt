package com.hawkeynl.jarvis.update

import com.hawkeynl.jarvis.network.AndroidUpdateMetadata
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import java.net.URI

const val ANDROID_CLIENT_PROTOCOL = 1
const val JARVIS_ANDROID_PACKAGE = "com.hawkeynl.jarvis"

sealed interface AndroidUpdateDecision {
    data object Current : AndroidUpdateDecision
    data class Available(val metadata: AndroidUpdateMetadata) : AndroidUpdateDecision
    data class Invalid(val reason: String) : AndroidUpdateDecision
}

object AndroidUpdatePolicy {
    private val digest = Regex("^[0-9a-f]{64}$")
    private val versionName = Regex(
        "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?$",
    )

    fun evaluate(
        endpoint: HomeNodeEndpoint,
        installedVersionCode: Int,
        installedSigningCertificateSha256: String,
        metadata: AndroidUpdateMetadata?,
        clientProtocol: Int = ANDROID_CLIENT_PROTOCOL,
    ): AndroidUpdateDecision {
        if (metadata == null) return AndroidUpdateDecision.Current
        if (metadata.schema_version != 1 || metadata.platform != "android") {
            return AndroidUpdateDecision.Invalid("Onbekend Android-updateformaat")
        }
        if (metadata.package_name != JARVIS_ANDROID_PACKAGE) {
            return AndroidUpdateDecision.Invalid("Update is niet voor de Jarvis-app")
        }
        if (metadata.version_code <= installedVersionCode) {
            return AndroidUpdateDecision.Invalid("Home Node bood een verouderde Android-versie aan")
        }
        if (metadata.version_code <= 0 || !versionName.matches(metadata.version_name)) {
            return AndroidUpdateDecision.Invalid("Ongeldige Android-versie")
        }
        if (metadata.minimum_client_protocol <= 0 || metadata.minimum_client_protocol > clientProtocol) {
            return AndroidUpdateDecision.Invalid("Deze update vereist een nieuwer Jarvis-updateprotocol")
        }
        if (!isEnrolledDownloadUrl(endpoint, metadata.download_url)) {
            return AndroidUpdateDecision.Invalid("Ongeldig Android-updatedownloadadres")
        }
        if (metadata.artifact.size <= 0 || metadata.artifact.size > MAX_APK_BYTES) {
            return AndroidUpdateDecision.Invalid("Ongeldige Android-updategrootte")
        }
        if (!digest.matches(metadata.artifact.sha256)) {
            return AndroidUpdateDecision.Invalid("Ongeldige APK-controlehash")
        }
        if (!digest.matches(metadata.artifact.signing_certificate_sha256) ||
            metadata.artifact.signing_certificate_sha256 != installedSigningCertificateSha256.lowercase()
        ) {
            return AndroidUpdateDecision.Invalid("APK is niet ondertekend door de geïnstalleerde Jarvis-identiteit")
        }
        return AndroidUpdateDecision.Available(metadata)
    }

    const val MAX_APK_BYTES: Long = 512L * 1024 * 1024

    private fun isEnrolledDownloadUrl(endpoint: HomeNodeEndpoint, value: String): Boolean {
        val enrolled = runCatching { URI(endpoint.baseUrl) }.getOrNull() ?: return false
        val download = runCatching { URI(value) }.getOrNull() ?: return false
        if (download.userInfo != null || download.query != null || download.fragment != null ||
            download.path != "/v1/app-updates/android/download"
        ) {
            return false
        }
        fun origin(uri: URI): Triple<String, String, Int>? {
            val scheme = uri.scheme?.lowercase() ?: return null
            val host = uri.host?.lowercase() ?: return null
            val port = if (uri.port >= 0) uri.port else when (scheme) {
                "https" -> 443
                "http" -> 80
                else -> return null
            }
            return Triple(scheme, host, port)
        }
        return origin(download) == origin(enrolled)
    }
}
