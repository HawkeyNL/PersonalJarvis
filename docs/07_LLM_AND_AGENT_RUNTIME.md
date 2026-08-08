# LLM- en agentruntime

## Providerstrategie

Gebruik één primaire cloudprovider en één lokale fallback.

### Aanbevolen start

- Primair: OpenAI óf Claude.
- Fallback/privacy: Ollama.
- Tweede cloudprovider pas toevoegen wanneer een concrete taak structureel beter of goedkoper is.

## Adapterinterface

```rust
#[async_trait]
pub trait ModelProvider {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse>;
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
    fn capabilities(&self) -> ModelCapabilities;
}
```

Capabilities:

- text
- vision
- structured output
- tool calling
- embeddings
- long context
- reasoning
- streaming

## Routing

De router kijkt naar:

- gevoeligheid;
- toolbehoefte;
- contextlengte;
- latency;
- maximale kosten;
- lokale beschikbaarheid;
- taakcomplexiteit.

## Rig

Rig kan nuttig zijn voor providerabstractie, structured output, tools en RAG. Houd domein- en risicologica echter buiten Rig zodat het framework vervangbaar blijft.

## Contextbeheer

- Stuur niet standaard volledige chatgeschiedenis.
- Maak compacte conversation summaries.
- Haal alleen taakrelevante memories op.
- Gebruik document-RAG met bronverwijzingen.
- Houd brokerstate uit vrije tekst en lever typed snapshots.
- Bewaar prompttemplates versiebaar.

## Structured outputs

Iedere agent levert een schema, bijvoorbeeld:

```json
{
  "summary": "...",
  "facts": [],
  "assumptions": [],
  "risks": [],
  "recommended_actions": [],
  "confidence": 0.0,
  "sources": []
}
```

Een ordervoorstel heeft een apart streng schema en kan niet uit gewone chattekst worden geëxecuteerd.

## Kostenbeheersing

- cache statische systeemprompts;
- dedupliceer nieuws;
- batchclassificatie;
- goedkope modellen voor triage;
- duur model alleen voor belangrijke analyses;
- harde token- en eurobudgetten per agent/job;
- contextcompressie;
- lokaal model voor simpele privacygevoelige taken.
