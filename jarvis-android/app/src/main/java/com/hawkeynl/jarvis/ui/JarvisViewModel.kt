package com.hawkeynl.jarvis.ui

import android.os.Build
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import com.hawkeynl.jarvis.AppContainer
import com.hawkeynl.jarvis.auth.EnrollmentOutcome
import com.hawkeynl.jarvis.network.ApiResult
import com.hawkeynl.jarvis.network.ConnectionState
import com.hawkeynl.jarvis.network.ConversationMessage
import com.hawkeynl.jarvis.network.ConversationSummary
import com.hawkeynl.jarvis.network.EndpointValidation
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import com.hawkeynl.jarvis.network.UnreachableReason
import com.hawkeynl.jarvis.security.BiometricAvailability
import com.hawkeynl.jarvis.security.BiometricResult
import java.time.Instant
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

enum class AppTab { CHAT, CONVERSATIONS, SETTINGS }

data class JarvisUiState(
    val selectedTab: AppTab = AppTab.CHAT,
    val endpoint: HomeNodeEndpoint? = null,
    val endpointDraft: String = "",
    val connection: ConnectionState = ConnectionState.NotConfigured,
    val locked: Boolean = false,
    val biometricMessage: String? = null,
    val authenticated: Boolean = false,
    val pairingPending: Boolean = false,
    val pairingExpiresAt: Long? = null,
    val conversations: List<ConversationSummary> = emptyList(),
    val conversationId: String? = null,
    val conversationTitle: String = "Nieuw gesprek",
    val messages: List<ConversationMessage> = emptyList(),
    val busy: Boolean = false,
    val error: String? = null,
)

class JarvisViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow(
        JarvisUiState(locked = container.sessions.hasSessionRecord()),
    )
    val state: StateFlow<JarvisUiState> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            container.settings.endpoint.collect { endpoint ->
                _state.update {
                    it.copy(
                        endpoint = endpoint,
                        endpointDraft = endpoint?.baseUrl ?: it.endpointDraft,
                        connection = if (endpoint == null) ConnectionState.NotConfigured else it.connection,
                    )
                }
                if (endpoint != null && !_state.value.locked) refreshAfterUnlock(endpoint)
            }
        }
    }

    fun selectTab(tab: AppTab) = _state.update { it.copy(selectedTab = tab) }
    fun editEndpoint(value: String) = _state.update { it.copy(endpointDraft = value, error = null) }

    fun saveEndpoint() {
        viewModelScope.launch {
            when (val result = container.settings.save(_state.value.endpointDraft)) {
                is EndpointValidation.Valid -> checkConnection(result.endpoint)
                is EndpointValidation.Invalid -> _state.update { it.copy(error = result.message) }
            }
        }
    }

    fun checkConnection() {
        val endpoint = _state.value.endpoint ?: return
        viewModelScope.launch { checkConnection(endpoint) }
    }

    fun beginEnrollment() {
        val endpoint = _state.value.endpoint ?: return
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            val result = container.enrollment.startOrResume(
                endpoint,
                "Jarvis Android (${Build.MODEL.take(40)})",
                Instant.now().epochSecond,
            )
            handleEnrollment(result)
            if (result is EnrollmentOutcome.Pending) pollPairingUntilResolved(endpoint)
        }
    }

    fun retryUnlockResult(result: BiometricResult) {
        when (result) {
            BiometricResult.Authenticated -> {
                _state.update { it.copy(locked = false, biometricMessage = null) }
                _state.value.endpoint?.let { endpoint ->
                    viewModelScope.launch { refreshAfterUnlock(endpoint) }
                }
            }
            BiometricResult.Cancelled -> _state.update { it.copy(biometricMessage = "Ontgrendeling geannuleerd.") }
            BiometricResult.Failed -> _state.update { it.copy(biometricMessage = "Biometrie niet herkend. Probeer opnieuw.") }
            BiometricResult.LockedOut -> _state.update { it.copy(biometricMessage = "Biometrie is tijdelijk geblokkeerd.") }
            is BiometricResult.Unavailable -> _state.update {
                it.copy(biometricMessage = result.availability.message())
            }
        }
    }

    fun openConversation(id: String) {
        val endpoint = _state.value.endpoint ?: return
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val result = container.conversations.load(endpoint, id)) {
                is ApiResult.Success -> _state.update {
                    it.copy(
                        selectedTab = AppTab.CHAT,
                        conversationId = result.value.id,
                        conversationTitle = result.value.title,
                        messages = result.value.messages,
                        busy = false,
                    )
                }
                else -> handleApiFailure(result)
            }
        }
    }

    fun newConversation() = _state.update {
        it.copy(
            selectedTab = AppTab.CHAT,
            conversationId = null,
            conversationTitle = "Nieuw gesprek",
            messages = emptyList(),
            error = null,
        )
    }

    fun send(text: String) {
        val endpoint = _state.value.endpoint ?: return
        val current = _state.value
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            val history = current.messages.map { message ->
                com.hawkeynl.jarvis.network.ChatTurn(message.role, message.content)
            }
            when (val result = container.conversations.send(
                endpoint,
                current.conversationId,
                history,
                text,
            )) {
                is ApiResult.Success -> {
                    val now = Instant.now().toString()
                    _state.update {
                        it.copy(
                            conversationId = result.value.conversation_id,
                            conversationTitle = result.value.conversation_title,
                            messages = it.messages + listOf(
                                ConversationMessage("user", text.trim(), at = now),
                                ConversationMessage("assistant", result.value.reply, result.value.model, now),
                            ),
                            busy = false,
                        )
                    }
                    loadConversations(endpoint)
                }
                else -> handleApiFailure(result)
            }
        }
    }

    fun logout() {
        val endpoint = _state.value.endpoint ?: return
        viewModelScope.launch {
            container.enrollment.logout(endpoint)
            _state.update { it.copy(authenticated = false, messages = emptyList(), conversations = emptyList()) }
        }
    }

    fun lockForBackground() {
        if (container.sessions.hasSessionRecord()) _state.update { it.copy(locked = true) }
    }

    fun resetDevice() {
        viewModelScope.launch {
            container.enrollment.resetDevice(_state.value.endpoint)
            _state.update {
                JarvisUiState(endpoint = it.endpoint, endpointDraft = it.endpointDraft, connection = it.connection)
            }
        }
    }

    private suspend fun refreshAfterUnlock(endpoint: HomeNodeEndpoint) {
        checkConnection(endpoint)
        val session = container.enrollment.currentSession()
        when {
            session.deviceId == null -> {
                val pending = container.sessions.pairingTicket()
                _state.update {
                    it.copy(
                        authenticated = false,
                        pairingPending = pending != null,
                        pairingExpiresAt = pending?.expires_at,
                    )
                }
                if (pending != null) pollPairingUntilResolved(endpoint)
            }
            session.token == null || session.expiresAt?.let { it <= Instant.now().epochSecond } != false ->
                handleEnrollment(container.enrollment.login(endpoint))
            else -> {
                _state.update { it.copy(authenticated = true) }
                loadConversations(endpoint)
            }
        }
    }

    private suspend fun checkConnection(endpoint: HomeNodeEndpoint) {
        _state.update { it.copy(connection = ConnectionState.Checking) }
        _state.update { state ->
            when (val result = container.api.ready(endpoint)) {
                is ApiResult.Success -> state.copy(connection = ConnectionState.Reachable(result.value.status))
                is ApiResult.Unreachable -> state.copy(connection = ConnectionState.Unreachable(result.reason))
                is ApiResult.HttpError -> state.copy(connection = ConnectionState.Rejected(result.status))
                ApiResult.Unauthorized -> state.copy(connection = ConnectionState.Rejected(401))
                is ApiResult.InvalidResponse -> state.copy(
                    connection = ConnectionState.Rejected(200),
                    error = result.message,
                )
            }
        }
    }

    private suspend fun pollPairingUntilResolved(endpoint: HomeNodeEndpoint) {
        while (_state.value.pairingPending) {
            val expiry = _state.value.pairingExpiresAt ?: break
            if (expiry <= Instant.now().epochSecond) {
                handleEnrollment(EnrollmentOutcome.Expired)
                break
            }
            delay(3_000)
            handleEnrollment(container.enrollment.poll(endpoint))
        }
    }

    private suspend fun handleEnrollment(result: EnrollmentOutcome) {
        when (result) {
            is EnrollmentOutcome.Pending -> _state.update {
                it.copy(
                    busy = false,
                    pairingPending = true,
                    pairingExpiresAt = result.ticket.expires_at,
                    error = null,
                )
            }
            is EnrollmentOutcome.Authenticated -> {
                _state.update {
                    it.copy(
                        busy = false,
                        pairingPending = false,
                        authenticated = true,
                        locked = true,
                        error = null,
                    )
                }
            }
            EnrollmentOutcome.Denied -> enrollmentError("Koppelverzoek geweigerd.")
            EnrollmentOutcome.Expired -> enrollmentError("Koppelverzoek verlopen. Start opnieuw.")
            EnrollmentOutcome.Unauthorized -> enrollmentError("Dit apparaat is niet meer geautoriseerd.")
            is EnrollmentOutcome.Unreachable -> enrollmentError(result.reason.message())
            is EnrollmentOutcome.Rejected -> enrollmentError(result.message ?: "Home Node weigerde het verzoek (${result.status}).")
            is EnrollmentOutcome.InvalidResponse -> enrollmentError(result.message)
        }
    }

    private fun enrollmentError(message: String) = _state.update {
        it.copy(busy = false, pairingPending = false, error = message)
    }

    private suspend fun loadConversations(endpoint: HomeNodeEndpoint) {
        when (val result = container.conversations.list(endpoint)) {
            is ApiResult.Success -> _state.update { it.copy(conversations = result.value.conversations) }
            else -> handleApiFailure(result)
        }
    }

    private fun handleApiFailure(result: ApiResult<*>) = _state.update { state ->
        when (result) {
            ApiResult.Unauthorized -> state.copy(
                authenticated = false,
                busy = false,
                error = "Sessie verlopen. Meld dit apparaat opnieuw aan.",
            )
            is ApiResult.Unreachable -> state.copy(
                connection = ConnectionState.Unreachable(result.reason),
                busy = false,
                error = result.reason.message(),
            )
            is ApiResult.HttpError -> state.copy(busy = false, error = result.message ?: "HTTP ${result.status}")
            is ApiResult.InvalidResponse -> state.copy(busy = false, error = result.message)
            is ApiResult.Success -> state
        }
    }

    companion object {
        fun factory(container: AppContainer): ViewModelProvider.Factory =
            object : ViewModelProvider.Factory {
                @Suppress("UNCHECKED_CAST")
                override fun <T : ViewModel> create(modelClass: Class<T>): T =
                    JarvisViewModel(container) as T
            }
    }
}

private fun UnreachableReason.message(): String = when (this) {
    UnreachableReason.TIMEOUT -> "Home Node antwoordt niet binnen de tijdslimiet."
    UnreachableReason.DNS -> "De hostnaam van de Home Node kan niet worden gevonden."
    UnreachableReason.TLS -> "De beveiligde verbinding met de Home Node is ongeldig."
    UnreachableReason.REFUSED -> "Home Node weigert de verbinding."
    UnreachableReason.NETWORK -> "Home Node is niet bereikbaar. Controleer wifi en het adres."
}

private fun BiometricAvailability.message(): String = when (this) {
    BiometricAvailability.Available -> "Biometrie beschikbaar."
    BiometricAvailability.NotEnrolled -> "Stel eerst sterke biometrie in bij Android-instellingen."
    BiometricAvailability.NoHardware -> "Dit toestel heeft geen ondersteunde sterke biometrie."
    BiometricAvailability.TemporarilyUnavailable -> "Biometrie is tijdelijk niet beschikbaar."
    BiometricAvailability.SecurityUpdateRequired -> "Installeer de Android-beveiligingsupdate voor biometrie."
    BiometricAvailability.Unsupported -> "Sterke biometrie wordt niet ondersteund op dit toestel."
}
