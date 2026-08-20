# Runbook — brokerverbinding weg

1. Blokkeer nieuwe submits.
2. Zet status op degraded.
3. Controleer of een request vlak vóór disconnect is verstuurd.
4. Reconcile via brokerorder-/executionhistory vóór retry.
5. Stuur nooit blind dezelfde order opnieuw.
6. Waarschuw gebruiker bij onbekende status.
7. Herstel sessie.
8. Reconcile positions, cash, orders en executions.
9. Hef blokkade pas op na consistente snapshot.
10. Leg incident vast.
