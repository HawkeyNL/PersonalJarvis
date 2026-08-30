package com.hawkeynl.jarvis.storage

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import com.hawkeynl.jarvis.network.EndpointValidation
import com.hawkeynl.jarvis.network.HomeNodeEndpoint
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

private val Context.jarvisSettings by preferencesDataStore(name = "jarvis_settings")

class EndpointSettingsRepository(private val context: Context) {
    val endpoint: Flow<HomeNodeEndpoint?> = context.jarvisSettings.data.map { preferences ->
        when (val parsed = HomeNodeEndpoint.parse(preferences[ENDPOINT].orEmpty())) {
            is EndpointValidation.Valid -> parsed.endpoint
            is EndpointValidation.Invalid -> null
        }
    }

    suspend fun save(raw: String): EndpointValidation {
        val validation = HomeNodeEndpoint.parse(raw)
        if (validation is EndpointValidation.Valid) {
            context.jarvisSettings.edit { it[ENDPOINT] = validation.endpoint.baseUrl }
        }
        return validation
    }

    private companion object {
        val ENDPOINT = stringPreferencesKey("home_node_endpoint")
    }
}
