package com.hawkeynl.jarvis.network

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.android.Android
import io.ktor.client.plugins.HttpRequestTimeoutException
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.plugins.timeout
import io.ktor.client.network.sockets.ConnectTimeoutException
import io.ktor.client.request.bearerAuth
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsChannel
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import io.ktor.serialization.kotlinx.json.json
import io.ktor.utils.io.jvm.javaio.toInputStream
import java.io.File
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.nio.channels.UnresolvedAddressException
import javax.net.ssl.SSLException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

class KtorJarvisApi(
    private val client: HttpClient = defaultHttpClient(),
) : JarvisApi {
    override suspend fun ready(endpoint: HomeNodeEndpoint) = request<HealthResponse> {
        client.get(endpoint.url("/readyz"))
    }

    override suspend fun createPairing(endpoint: HomeNodeEndpoint, request: PairingCreateRequest) =
        request<PairingTicket> {
            client.post(endpoint.url("/v1/auth/pairing/requests")) {
                contentType(ContentType.Application.Json)
                setBody(request)
            }
        }

    override suspend fun pairingStatus(endpoint: HomeNodeEndpoint, ticket: PairingTicket) =
        request<PairingStatusResponse> {
            client.get(endpoint.url("/v1/auth/pairing/requests/${ticket.request_id}/status")) {
                header("X-Jarvis-Pairing-Nonce", ticket.nonce)
            }
        }

    override suspend fun challenge(endpoint: HomeNodeEndpoint, request: ChallengeRequest) =
        request<ChallengeResponse> {
            client.post(endpoint.url("/v1/auth/challenge")) {
                contentType(ContentType.Application.Json)
                setBody(request)
            }
        }

    override suspend fun login(endpoint: HomeNodeEndpoint, request: LoginRequest) =
        request<LoginResponse> {
            client.post(endpoint.url("/v1/auth/login")) {
                contentType(ContentType.Application.Json)
                setBody(request)
            }
        }

    override suspend fun logout(endpoint: HomeNodeEndpoint, token: String) = request<Unit> {
        client.post(endpoint.url("/v1/auth/logout")) { bearerAuth(token) }
    }

    override suspend fun deleteDevice(endpoint: HomeNodeEndpoint, token: String, deviceId: String) =
        request<Unit> {
            client.delete(endpoint.url("/v1/devices/$deviceId")) { bearerAuth(token) }
        }

    override suspend fun conversations(endpoint: HomeNodeEndpoint, token: String) =
        request<ConversationListResponse> {
            client.get(endpoint.url("/v1/conversations")) { bearerAuth(token) }
        }

    override suspend fun conversation(endpoint: HomeNodeEndpoint, token: String, id: String) =
        request<ConversationDetailResponse> {
            client.get(endpoint.url("/v1/conversations/$id")) { bearerAuth(token) }
        }

    override suspend fun chat(endpoint: HomeNodeEndpoint, token: String, request: ChatRequest) =
        request<ChatResponse> {
            client.post(endpoint.url("/v1/assistant/chat")) {
                bearerAuth(token)
                contentType(ContentType.Application.Json)
                setBody(request)
            }
        }

    override suspend fun androidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        currentVersionCode: Int,
        clientProtocol: Int,
    ): ApiResult<AndroidUpdateMetadata?> {
        return try {
            val response = client.get(
                endpoint.url("/v1/app-updates/android/$currentVersionCode?client_protocol=$clientProtocol"),
            ) { bearerAuth(token) }
            when {
                response.status == HttpStatusCode.NoContent -> ApiResult.Success(null)
                response.status == HttpStatusCode.Unauthorized -> ApiResult.Unauthorized
                response.status.value !in 200..299 -> {
                    val error = runCatching { response.body<ErrorResponse>() }.getOrNull()
                    ApiResult.HttpError(response.status.value, error?.hint ?: error?.error)
                }
                else -> ApiResult.Success(response.body<AndroidUpdateMetadata>())
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: SerializationException) {
            ApiResult.InvalidResponse(error.message ?: "Ongeldige Android-updatemetadata")
        } catch (error: Exception) {
            networkFailure(error)
        }
    }

    override suspend fun downloadAndroidUpdate(
        endpoint: HomeNodeEndpoint,
        token: String,
        destination: File,
        expectedSize: Long,
    ): ApiResult<Unit> {
        return try {
            val response = client.get(endpoint.url("/v1/app-updates/android/download")) {
                bearerAuth(token)
                timeout {
                    requestTimeoutMillis = 5 * 60 * 1_000
                    socketTimeoutMillis = 30_000
                }
            }
            when {
                response.status == HttpStatusCode.Unauthorized -> ApiResult.Unauthorized
                response.status.value !in 200..299 -> {
                    val error = runCatching { response.body<ErrorResponse>() }.getOrNull()
                    ApiResult.HttpError(response.status.value, error?.hint ?: error?.error)
                }
                response.contentType()?.withoutParameters() != ANDROID_APK_CONTENT_TYPE ->
                    ApiResult.InvalidResponse("Home Node stuurde geen Android APK")
                response.headers[HttpHeaders.ContentLength]
                    ?.toLongOrNull()
                    ?.let { it != expectedSize } == true ->
                    ApiResult.InvalidResponse("Home Node stuurde een APK met een onverwachte grootte")
                else -> {
                    val exactSize = withContext(Dispatchers.IO) {
                        var written = 0L
                        destination.outputStream().use { output ->
                            response.bodyAsChannel().toInputStream().use { input ->
                                val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                                while (true) {
                                    val read = input.read(buffer)
                                    if (read < 0) break
                                    written += read
                                    if (written > expectedSize) return@withContext false
                                    output.write(buffer, 0, read)
                                }
                            }
                        }
                        written == expectedSize
                    }
                    if (!exactSize) {
                        destination.delete()
                        ApiResult.InvalidResponse("APK-download heeft niet de verwachte grootte")
                    } else {
                        ApiResult.Success(Unit)
                    }
                }
            }
        } catch (cancelled: CancellationException) {
            destination.delete()
            throw cancelled
        } catch (error: Exception) {
            destination.delete()
            networkFailure(error)
        }
    }

    private suspend inline fun <reified T> request(
        crossinline block: suspend () -> io.ktor.client.statement.HttpResponse,
    ): ApiResult<T> {
        return try {
            val response = block()
            when {
                response.status == HttpStatusCode.Unauthorized -> ApiResult.Unauthorized
                response.status.value !in 200..299 -> {
                    val error = runCatching { response.body<ErrorResponse>() }.getOrNull()
                    ApiResult.HttpError(response.status.value, error?.hint ?: error?.error)
                }
                T::class == Unit::class -> {
                    @Suppress("UNCHECKED_CAST")
                    ApiResult.Success(Unit as T)
                }
                else -> ApiResult.Success(response.body())
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: HttpRequestTimeoutException) {
            ApiResult.Unreachable(UnreachableReason.TIMEOUT)
        } catch (_: ConnectTimeoutException) {
            ApiResult.Unreachable(UnreachableReason.TIMEOUT)
        } catch (_: SocketTimeoutException) {
            ApiResult.Unreachable(UnreachableReason.TIMEOUT)
        } catch (_: UnresolvedAddressException) {
            ApiResult.Unreachable(UnreachableReason.DNS)
        } catch (_: UnknownHostException) {
            ApiResult.Unreachable(UnreachableReason.DNS)
        } catch (_: SSLException) {
            ApiResult.Unreachable(UnreachableReason.TLS)
        } catch (_: ConnectException) {
            ApiResult.Unreachable(UnreachableReason.REFUSED)
        } catch (error: SerializationException) {
            ApiResult.InvalidResponse(error.message ?: "Ongeldig antwoord van Home Node")
        } catch (_: Exception) {
            ApiResult.Unreachable(UnreachableReason.NETWORK)
        }
    }

    companion object {
        private val ANDROID_APK_CONTENT_TYPE = ContentType("application", "vnd.android.package-archive")

        private fun defaultHttpClient() = HttpClient(Android) {
            expectSuccess = false
            // Authenticated requests never follow redirects: bearer credentials
            // must remain bound to the enrolled Home Node origin.
            followRedirects = false
            install(HttpTimeout) {
                connectTimeoutMillis = 5_000
                requestTimeoutMillis = 30_000
                socketTimeoutMillis = 30_000
            }
            install(ContentNegotiation) {
                json(Json {
                    ignoreUnknownKeys = true
                    explicitNulls = false
                })
            }
            // Deliberately no HTTP logging plugin: bearer and pairing tokens must not reach logs.
        }
    }

    private fun networkFailure(error: Exception): ApiResult<Nothing> = when (error) {
        is HttpRequestTimeoutException, is ConnectTimeoutException, is SocketTimeoutException ->
            ApiResult.Unreachable(UnreachableReason.TIMEOUT)
        is UnresolvedAddressException, is UnknownHostException ->
            ApiResult.Unreachable(UnreachableReason.DNS)
        is SSLException -> ApiResult.Unreachable(UnreachableReason.TLS)
        is ConnectException -> ApiResult.Unreachable(UnreachableReason.REFUSED)
        is SerializationException -> ApiResult.InvalidResponse(
            error.message ?: "Ongeldig antwoord van Home Node",
        )
        else -> ApiResult.Unreachable(UnreachableReason.NETWORK)
    }
}

private fun HomeNodeEndpoint.url(path: String): String = "$baseUrl$path"
