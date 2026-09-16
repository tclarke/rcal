# Position & Heading Reporting for Autonomous Systems (ASK 6.0a)

**Source:** `specs/a-gra/Documentation/ASK_6.0a_C2_Interface_Volume.pdf`

---

## Primary Message: StatusReport

The autonomous system's SAT (Sensor/Actuator/Transceiver) component sends **StatusReport** messages to C2. This is the top-level "station-keeping" container — it reports platform state, task status, and autonomy status. Subtypes carry position/heading specifics.

> Details on timing and SAP vs. GCS variants: **C2_Interface_Volume.pdf §3.4 (pp. ~1–38)**

---

## Position/Heading Submessages

Three messages carry position and heading data:

### 1. `PositionReport` (`PositionReportMT`) — primary, station-keeping

Analogous to MIL-STD-6016 PPLI. Sent periodically by the autonomous system.

Key fields (`PositionReportMDT`):

| Field | Type | Notes |
|---|---|---|
| `SystemID` | required | identifies the reporting system |
| `Source` | `SystemSourceEnum` | e.g., measured, estimated |
| `CurrentOperatingDomain` | `EnvironmentEnum` | air/ground/sea/etc. |
| `InertialState` | `InertialStateType` | see below |
| `DisplayName` | optional | |

`InertialStateType` contains:
- `Position: Point4D_Type` — lat/lon/altitude + time
- `PositionUncertainty` (optional)
- `DomainVelocity: Velocity3D_Type` (optional) — body-frame speed
- `GroundVelocity: Velocity2D_Type` (optional) — course + ground speed
- `DomainAcceleration: Acceleration3D_Type` (optional)

The C2 volume (p. 50) also names these human-readable fields in subtypes: **Altitude (MSL)**, **GeodeticPoint (lat/lon)**, **Heading (degrees true)**, **Roll**, **Pitch**, **Course (degrees true)**, **Speed (m/s)**, **VerticalSpeed (m/s)**, **ElevationAngle**.

> Schema: `schema_summary.md` lines 4048–4049, 2917. C2 volume §1.3.2.9 (p. 420) for MA extension.

### 2. `NavigationReport` (`NavigationReportMT`) — station-keeping

Reports current navigation state (endurance, contingency level, nav solution source).

Key fields (`NavigationReportMDT`):
- `SystemID`, `Source`, `ContingencyLevel: SystemContingencyLevelEnum`, `Endurance`, `Navigation: NavigationSourceDetailsType` (optional)

> Schema: `schema_summary.md` line 3307. C2 volume §1.3.2.24 (p. 478) for MA extension.

### 3. `PositionReportDetailed` (`PositionReportDetailedMT`) — not station-keeping

Higher-fidelity position, supports up to 4 position sources simultaneously (e.g., GPS + INS fusion).

Key fields (`PositionReportDetailedMDT`):
- `PositionReportData: PositionReportDataType [1..4]` — each entry has: `PositionSource`, `NavigationSolutionState`, `FigureOfMerit`, kinematics
- `SimulationTargetNumber` (optional)

> Schema: `schema_summary.md` lines 4045–4047. C2 volume §1.3.2.89 (p. 606) for MA extension.

---

## Aggregate Type

`SystemDataType` bundles all of the above for system representation:

```
SystemStatus (required)
Position: PositionReportMDT (optional)
Navigation: NavigationReportMDT (optional)
Metadata (optional)
```

> Schema: `schema_summary.md` line 5254.

---

## Timing & Filtering

The C2 volume §3.4.3–3.4.4 specifies StatusReport send rates and filtering rules (SAP vs. GCS variants differ). Read **C2_Interface_Volume.pdf §3.4 (pp. ~1–38 front matter)** directly for timing requirements.

---

## Call/Response

Position and navigation reports are published (push), not polled. No request message is needed for normal operation. The one exception is **§1.2.7.19 "Share Vehicle State Data" (p. 135)** — C2 can request a state update, and the system responds with the StatusReport subtypes above.
