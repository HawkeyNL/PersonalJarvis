# Agentindex

Agents zijn rollen met strikte contracten, geen autonome microservices per se.

| Agent | Hoofdtaak | Mag muteren? |
|---|---|---|
| Orchestrator | routeert en plant | Nee |
| Memory Agent | relevante context | Beperkt |
| Investment Analyst | langetermijnanalyse | Nee |
| Market Research Agent | marktcontext | Nee |
| Trading Analyst | tradehypotheses | Nee |
| Risk Manager | deterministische beslissing | Policyservice, geen LLM |
| Execution Agent | approved command uitvoeren | Ja, streng |
| Trade Manager | approved beheerregels | Ja, streng |
| Backtest Agent | tests orkestreren/analyseren | Geen live |
| News Agent | nieuws triage | Nee |
| Content Orchestrator | contentworkflow | Drafts |
| Trend Scout | trendonderzoek | Nee |
| Shorts Producer | script/renderplanning | Drafts |
| Coding Agent | codehulp | Alleen sandbox/repo met toestemming |
| Personal Assistant | algemene workflows | Afhankelijk van tool |
| Security Guardian | policy/anomalieën | Mag blokkeren |

Iedere agentfile bevat: doel, inputs, outputs, tools, verboden acties en evaluatiecriteria.
