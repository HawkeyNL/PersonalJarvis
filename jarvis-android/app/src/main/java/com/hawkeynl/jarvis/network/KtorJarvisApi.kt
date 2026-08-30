package com.hawkeynl.jarvis.network

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.android.Android
import io.ktor.client.plugins.HttpRequestTimeoutException
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.network.sockets.ConnectTimeoutException
import io.ktor.client.request.bearerAuth
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.serialization.kotlinx.json.json
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.nio.channels.UnresolvedAddressException
import javax.net.ssl.SSLException
import kotlinx.coroutines.CancellationException
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
        private fun defaultHttpClient() = HttpClient(Android) {
            expectSuccess = false
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
}

private fun HomeNodeEndpoint.url(path: String): String = "$baseUrl$path"
