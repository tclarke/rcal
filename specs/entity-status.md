# A-GRA: Reporting Known Entities

Primary document: **ASK_6.0a_Peer_Interface_Volume.pdf** (Peer L1 Interface Volume)

---

## Background: Fused vs. Un-fused Tracks

An onboard sensor tracker links Observation Measurement Reports (OMRs) to produce **un-fused entities**. A sensor fusion engine then ingests those to produce **fused entities (tracks)**. Both types are shared over the P2P interface. (Peer Vol, pp. 81–82)

---

## 1. Distribute Sensor Track Data (§1.2.5.4, pp. 81–84)

**Purpose**: Share sensor tracks (fused or un-fused) between ACPs to enable package-level fusion and update the Common Operating Picture (COP).

| Message | Required Fields |
|---|---|
| `EntityMT` (un-fused, `EntityMT_1`) | `MeasurementID` |
| `EntityMT` (fused, `EntityMT_2`) | `MeasurementID`, `SourceTypeIdentifier` |
| `ObservationMeasurementReportMT` *(optional, if bandwidth allows)* | `MeasurementSource` |

For `ObservationMeasurementReportMT`, two source paths matter:
- **Kinematics source**: `.MeasurementSource.ElementDetails.SourceElementIdentifier.SystemID + SubsystemID`
- **Identity source**: `.MeasurementSource.SourceIdentity.CapabilityID + SystemID + SubsystemID`

Fused vs. un-fused is distinguished by `SourceTypeIdentifier.Fusion`: present on fused tracks (lists contributing un-fused entities), absent on un-fused tracks.

---

## 2. Synchronize Global COP to Peer (§1.2.5.5, pp. 84–85)

**Purpose**: COP Leader distributes updated Global COP (entity data + team states) to all COP Followers each time the COP updates.

| Message | Required Fields |
|---|---|
| `EntityMT` | `MeasurementID`, `SourceTypeIdentifier` |
| `PackageStatusMT` | None |

---

## 3. Send Entity to Peer (§1.2.10.11.8, p. 150)

**Purpose**: General-purpose entity share to a peer (used within COP data distribution flows).

| Message | Required Fields |
|---|---|
| `EntityMT` | `ObjectState` |

`ObjectState` distinguishes NEW / UPDATED / REMOVED lifecycle. When sending REMOVED, set `RemoveInfo` (see MP Vol, p. 108 for `EntityRemoveInfoType`).

---

## 4. Query Entity History (§1.2.15.8, pp. 259–260) — Call/Response

**Purpose**: MA (Flight Lead) requests historical entity data from a peer node.

**Call**:

| Message | Required Fields |
|---|---|
| `QueryDataRequestMT` | None required beyond standard |

Uses `QueryMessage.MessageType = "ENTITY"` with a `QueryType` filter (e.g., by `EntityID`). Can combine ENTITY + EntityPropagation in one query. (See XML example, Peer Vol p. 259)

**Response**: `QueryDataRequestStatusMT` (4 variants: success/cannot-comply × with-reason/without). No additional response beyond the status message.

---

## 5. Query Entity Track History (§1.2.15.9, pp. 261–263) — Call/Response

**Purpose**: MA requests historical track + propagation data from a peer for weapon employment and track display.

**Call**:

| Message | Required Fields |
|---|---|
| `QueryDataRequestMT` | `Query: QueryMessageType` |

**Response**: `QueryDataRequestStatusMT` (same 4 variants as above).

Track propagation data includes extrapolated kinematics (predicted positions). The fusion engine takes COP + local sensor data to build track history.

---

## Notes

- The `EntityMT` `Strength` field (optional) conveys signal strength. (MP Vol, p. 107)
- `MeasurementID` on `EntityMT` links back to the contributing OMRs.
- Vehicle state (ownship position, speed, etc.) is reported separately via `MA_PositionReportMT`, `MA_NavigationReportMT`, etc. — not covered here.
- C2 Interface Volume (`ASK_6.0a_C2_Interface_Volume.pdf`) likely covers entity reporting upward to C2; that PDF did not finish processing — check it directly if uplink reporting to C2 is needed.
