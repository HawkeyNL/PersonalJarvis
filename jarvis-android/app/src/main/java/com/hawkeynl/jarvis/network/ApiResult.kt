package com.hawkeynl.jarvis.network

sealed interface ApiResult<out T> {
    data class Success<T>(val value: T) : ApiResult<T>
    data object Unauthorized : ApiResult<Nothing>
    data class HttpError(val status: Int, val message: String?) : ApiResult<Nothing>
    data class Unreachable(val reason: UnreachableReason) : ApiResult<Nothing>
    data class InvalidResponse(val message: String) : ApiResult<Nothing>
}

enum class UnreachableReason {
    TIMEOUT,
    DNS,
    TLS,
    REFUSED,
    NETWORK,
}

sealed interface ConnectionState {
    data object NotConfigured : ConnectionState
    data object Checking : ConnectionState
    data class Reachable(val status: String) : ConnectionState
    data class Unreachable(val reason: UnreachableReason) : ConnectionState
    data class Rejected(val status: Int) : ConnectionState
}
