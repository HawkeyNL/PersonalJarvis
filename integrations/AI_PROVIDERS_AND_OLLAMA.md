# AI-providers en Ollama

## Eenvoudige setup

- Eén primaire cloud-API: OpenAI of Anthropic.
- Ollama als lokale fallback.
- Provideradapter in Rust.
- Geen providerspecifieke types in domeinlogica.

## Keuzecriteria

- structured output
- tool calling
- latency
- context
- prijs
- privacy
- modelstabiliteit
- rate limits
- evaluatieresultaten op eigen taken

## Ollama

Gebruik lokaal voor:

- classificatie;
- samenvattingen;
- notities;
- eenvoudige codehulp;
- offline chat;
- privacygevoelige low-risk taken.

Gebruik een lokaal model niet automatisch voor financiële executionbeslissingen. Modelnaam alleen is onvoldoende; benchmark exact model, quantization en hardware.

## Evaluatieharnas

Maak een vaste suite met:

- 50 portfolio/news cases;
- 50 tool-selection cases;
- 30 prompt-injection cases;
- 30 coding tasks;
- 30 content scripts;
- structured-output validity;
- hallucination rate;
- cost and latency.

Routeer pas op basis van eigen meetgegevens.
