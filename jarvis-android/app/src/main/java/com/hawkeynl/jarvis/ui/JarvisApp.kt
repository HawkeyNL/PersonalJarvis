package com.hawkeynl.jarvis.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.hawkeynl.jarvis.network.ConnectionState
import com.hawkeynl.jarvis.network.ConversationMessage

@Composable
fun JarvisApp(
    state: JarvisUiState,
    actions: JarvisViewModel,
    onRequestBiometric: () -> Unit,
    onInstallUpdate: () -> Unit,
) {
    when {
        state.locked -> AppLockScreen(
            message = state.biometricMessage,
            onRetry = onRequestBiometric,
            onReset = actions::resetDevice,
        )
        state.endpoint == null || !state.authenticated -> OnboardingScreen(state, actions)
        else -> AuthenticatedShell(state, actions, onInstallUpdate)
    }
}

@Composable
private fun AppLockScreen(message: String?, onRetry: () -> Unit, onReset: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("J", style = MaterialTheme.typography.displayLarge, color = MaterialTheme.colorScheme.primary)
        Text("Jarvis is vergrendeld", style = MaterialTheme.typography.headlineSmall)
        Text(
            message ?: "Bevestig met sterke biometrie om je sessie te openen.",
            modifier = Modifier.padding(vertical = 16.dp),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Button(onClick = onRetry) { Text("Opnieuw ontgrendelen") }
        OutlinedButton(onClick = onReset, modifier = Modifier.padding(top = 24.dp)) {
            Text("Wis dit apparaat en koppel opnieuw")
        }
    }
}

@Composable
private fun OnboardingScreen(state: JarvisUiState, actions: JarvisViewModel) {
    LazyColumn(
        modifier = Modifier.fillMaxSize().imePadding().testTag("onboarding"),
        contentPadding = PaddingValues(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        item {
            Text("Jarvis", style = MaterialTheme.typography.displaySmall, fontWeight = FontWeight.Bold)
            Text(
                "Verbind deze telefoon met je Home Node en laat een bestaand vertrouwd apparaat de koppeling goedkeuren.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        item {
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text("Home Node", style = MaterialTheme.typography.titleMedium)
                    OutlinedTextField(
                        value = state.endpointDraft,
                        onValueChange = actions::editEndpoint,
                        modifier = Modifier.fillMaxWidth().testTag("endpoint"),
                        label = { Text("https://jarvis.local") },
                        singleLine = true,
                        keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(imeAction = ImeAction.Done),
                        keyboardActions = KeyboardActions(onDone = { actions.saveEndpoint() }),
                    )
                    Button(onClick = actions::saveEndpoint, enabled = !state.busy) {
                        Text("Opslaan en controleren")
                    }
                    ConnectionLine(state.connection)
                }
            }
        }
        if (state.endpoint != null) {
            item {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        Text("Apparaat koppelen", style = MaterialTheme.typography.titleMedium)
                        if (state.pairingPending) {
                            Text("Wacht op goedkeuring vanaf een vertrouwd Jarvis-apparaat.")
                            Text(
                                "Verloopt: ${state.pairingExpiresAt ?: "—"}",
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                fontFamily = FontFamily.Monospace,
                            )
                            CircularProgressIndicator()
                        } else {
                            Button(onClick = actions::beginEnrollment, enabled = !state.busy) {
                                Text(if (state.busy) "Bezig…" else "Koppel deze telefoon")
                            }
                        }
                    }
                }
            }
        }
        state.error?.let { error ->
            item { Text(error, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("error")) }
        }
    }
}

@Composable
private fun AuthenticatedShell(
    state: JarvisUiState,
    actions: JarvisViewModel,
    onInstallUpdate: () -> Unit,
) {
    Scaffold(
        bottomBar = {
            NavigationBar(modifier = Modifier.testTag("bottom-navigation")) {
                listOf(
                    AppTab.CHAT to ("●" to "Chat"),
                    AppTab.CONVERSATIONS to ("≡" to "Gesprekken"),
                    AppTab.SETTINGS to ("⚙" to "Instellingen"),
                ).forEach { (tab, presentation) ->
                    NavigationBarItem(
                        selected = state.selectedTab == tab,
                        onClick = { actions.selectTab(tab) },
                        icon = { Text(presentation.first) },
                        label = { Text(presentation.second) },
                    )
                }
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            ConnectionBanner(state.connection, actions::checkConnection)
            state.error?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(12.dp)) }
            when (state.selectedTab) {
                AppTab.CHAT -> ChatScreen(state, actions)
                AppTab.CONVERSATIONS -> ConversationsScreen(state, actions)
                AppTab.SETTINGS -> SettingsScreen(state, actions, onInstallUpdate)
            }
        }
    }
}

@Composable
private fun ConnectionLine(connection: ConnectionState) {
    Text(
        when (connection) {
            ConnectionState.NotConfigured -> "Nog niet ingesteld"
            ConnectionState.Checking -> "Verbinding controleren…"
            is ConnectionState.Reachable -> "Verbonden · ${connection.status}"
            is ConnectionState.Unreachable -> "Niet bereikbaar · ${connection.reason.name.lowercase()}"
            is ConnectionState.Rejected -> "Home Node antwoordde met HTTP ${connection.status}"
        },
        color = when (connection) {
            is ConnectionState.Reachable -> MaterialTheme.colorScheme.primary
            is ConnectionState.Unreachable, is ConnectionState.Rejected -> MaterialTheme.colorScheme.error
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        },
    )
}

@Composable
private fun ConnectionBanner(connection: ConnectionState, retry: () -> Unit) {
    if (connection is ConnectionState.Reachable) return
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Box(Modifier.weight(1f)) { ConnectionLine(connection) }
        OutlinedButton(onClick = retry) { Text("Opnieuw") }
    }
    HorizontalDivider()
}

@Composable
private fun ChatScreen(state: JarvisUiState, actions: JarvisViewModel) {
    var draft by remember(state.conversationId) { mutableStateOf("") }
    Column(Modifier.fillMaxSize().imePadding().testTag("chat")) {
        Text(
            state.conversationTitle,
            modifier = Modifier.padding(16.dp),
            style = MaterialTheme.typography.titleLarge,
        )
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (state.messages.isEmpty()) item { Text("Waarmee kan ik helpen?") }
            items(state.messages) { message -> MessageBubble(message) }
        }
        Row(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.Bottom,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                modifier = Modifier.weight(1f).testTag("composer"),
                placeholder = { Text("Bericht aan Jarvis") },
                maxLines = 5,
            )
            Button(
                onClick = {
                    val message = draft
                    draft = ""
                    actions.send(message)
                },
                enabled = draft.isNotBlank() && !state.busy,
            ) { Text("Stuur") }
        }
    }
}

@Composable
private fun MessageBubble(message: ConversationMessage) {
    val jarvis = message.role == "assistant" || message.role == "jarvis"
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Text(if (jarvis) "JARVIS" else "JIJ", color = MaterialTheme.colorScheme.primary)
            Text(message.content)
        }
    }
}

@Composable
private fun ConversationsScreen(state: JarvisUiState, actions: JarvisViewModel) {
    LazyColumn(
        modifier = Modifier.fillMaxSize().testTag("conversations"),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("Gesprekken", style = MaterialTheme.typography.headlineSmall)
                Button(onClick = actions::newConversation) { Text("Nieuw") }
            }
        }
        items(state.conversations, key = { it.id }) { conversation ->
            Card(onClick = { actions.openConversation(conversation.id) }, modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(14.dp)) {
                    Text(conversation.title, fontWeight = FontWeight.SemiBold)
                    Text(
                        conversation.updated_at,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        }
    }
}

@Composable
private fun SettingsScreen(
    state: JarvisUiState,
    actions: JarvisViewModel,
    onInstallUpdate: () -> Unit,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize().imePadding().testTag("settings"),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { Text("Instellingen", style = MaterialTheme.typography.headlineSmall) }
        item {
            OutlinedTextField(
                value = state.endpointDraft,
                onValueChange = actions::editEndpoint,
                modifier = Modifier.fillMaxWidth(),
                label = { Text("Home Node-adres") },
                singleLine = true,
            )
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = actions::saveEndpoint) { Text("Opslaan") }
                OutlinedButton(onClick = actions::checkConnection) { Text("Test") }
            }
        }
        item { ConnectionLine(state.connection) }
        item {
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Jarvis Android", style = MaterialTheme.typography.titleMedium)
                    when (val update = state.appUpdate) {
                        AndroidUpdateUiState.Idle -> Text("Update nog niet gecontroleerd")
                        AndroidUpdateUiState.Checking -> Text("Update controleren…")
                        AndroidUpdateUiState.Current -> Text("App is bijgewerkt")
                        is AndroidUpdateUiState.Available -> {
                            Text("Versie ${update.metadata.version_name} is beschikbaar")
                            Button(onClick = actions::downloadUpdate) { Text("Download update") }
                        }
                        AndroidUpdateUiState.Downloading -> Text("APK downloaden en controleren…")
                        is AndroidUpdateUiState.Ready -> {
                            Text("Versie ${update.versionName} is gecontroleerd en klaar voor installatie")
                            Button(onClick = onInstallUpdate) { Text("Open Android-installatie") }
                        }
                        AndroidUpdateUiState.PermissionRequired -> Text(
                            "Geef Jarvis in Android-instellingen toestemming om deze gecontroleerde APK te installeren en probeer opnieuw.",
                            color = MaterialTheme.colorScheme.error,
                        )
                        is AndroidUpdateUiState.Failed -> Text(
                            update.message,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    OutlinedButton(
                        onClick = actions::checkForUpdate,
                        enabled = state.appUpdate !is AndroidUpdateUiState.Checking &&
                        state.appUpdate !is AndroidUpdateUiState.Downloading,
                    ) { Text("Opnieuw controleren") }
                    if (state.appUpdate is AndroidUpdateUiState.PermissionRequired) {
                        Button(onClick = onInstallUpdate) { Text("Installatie opnieuw openen") }
                    }
                }
            }
        }
        item { Spacer(Modifier.height(8.dp)) }
        item { OutlinedButton(onClick = actions::logout) { Text("Uitloggen") } }
        item { OutlinedButton(onClick = actions::resetDevice) { Text("Apparaat wissen en opnieuw koppelen") } }
    }
}
