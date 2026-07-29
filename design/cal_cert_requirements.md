# OMS CAL CERT Requirements
**Source:** OMSC-SPC-001 Rev L — Open Mission Systems CAL Specification, 22 January 2026
**Total CERTs extracted:** 55

| Requirement ID | Doc Page | Requirement Description |
|---|---|---|
| CAL-005179 | 10 | A C++ CAL Implementation shall implement the methods defined by the C++ CAL Interface Generation Specification (document OMSC-SPC-008). |
| CAL-005180 | 10 | A Java CAL Implementation shall implement the methods defined by the Java CAL Interface Generation Specification (document OMSC-SPC-007). |
| CAL-005181 | 14 | A CAL Implementation shall generate UUIDs in accordance with the RFC 4122, UUID version 1, 3, and 4. |
| CAL-005201 | 14 | A CAL Implementation shall have a mechanism for the CAL Client to obtain a fully initialized instance of the CAL associated with a Service Identifier. |
| CAL-005202 | 15 | A CAL Implementation shall associate a single CAL instance with each unique combination of Service Identifier and ASB Identifier. |
| CAL-005203 | 15 | A CAL Implementation shall have a mechanism for retrieving the UUIDs that identify the System, Service, Subsystem, Components, and Capabilities associated with the initializing Service. |
| CAL-005204 | 15 | A CAL Implementation shall report an error to the CAL Client in the event the initialization of the CAL instance fails. |
| CAL-005208 | 15 | A CAL Implementation shall associate a topic with one and only one CAL Message type. |
| CAL-005209 | 15 | A CAL Implementation shall map Client Topics to the applicable CAL Topics. |
| CAL-005210 | 16 | A CAL Implementation shall support the configuration of independent Quality of Service settings for each Client Topic. |
| CAL-005254 | 18 | A CAL Implementation shall create optional fields in a disabled state. |
| CAL-005264 | 19 | CAL Implementation shall create bounded lists in an empty state. |
| CAL-005267 | 19 | A CAL Implementation shall create choice fields in an uninitialized state. |
| CAL-005275 | 19 | A CAL Implementation shall have a mechanism for destroying CAL Messages and Sub-messages. |
| CAL-005290 | 20 | A CAL Implementation shall have a mechanism to enable or disable optional fields. |
| CAL-005293 | 20 | A CAL Implementation shall have a mechanism to determine the enabled or disabled state of an optional field. |
| CAL-005294 | 20 | A CAL Implementation shall indicate to the CAL Client that a field is disabled when a disabled optional field is accessed. |
| CAL-005296 | 20 | A CAL Implementation shall provide the value of the field when the value of an enabled optional field is accessed. |
| CAL-005364 | 22 | A CAL Implementation shall have a mechanism to create Writers associated with a Client Topic. |
| CAL-005368 | 22 | A CAL Writer Instance shall be associated with one and only one Client Topic. |
| CAL-005369 | 22 | A CAL Writer Instance shall report an error upon invocation of the write operation when the associated Client Topic is unavailable. |
| CAL-005374 | 23 | A CAL Implementation shall have a mechanism to create Readers associated with a Client Topic. |
| CAL-005378 | 23 | A CAL Reader Instance shall be associated with one and only one Client Topic. |
| CAL-005379 | 23 | A CAL Reader Implementation shall provide a callback interface for receiving CAL Messages. |
| CAL-005380 | 23 | A CAL Reader Implementation shall provide a polling interface for receiving CAL Messages. |
| CAL-005391 | 24 | A CAL Reader Implementation shall allow a CAL Client to register zero or more listener instances for receiving incoming CAL Messages. |
| CAL-005392 | 24 | A CAL Reader Implementation shall provide a CAL Client with a single, shared CAL Message reference by invoking the message handler operation of each registered listener instance one time for each received CAL Message instance. |
| CAL-005394 | 24 | A CAL Reader Implementation shall establish the topic connection when the Reader is created. |
| CAL-005396 | 25 | A CAL Implementation shall provide the ability to unregister a listener instance. |
| CAL-005431 | 26 | A CAL Reader Instance shall provide Time Based Filter Quality of Service such that the CAL Reader drops CAL Messages received on a Client Topic within a specified time period since the last CAL Message was accepted on the Client Topic. |
| CAL-005434 | 26 | A CAL Implementation shall provide the Reliability Quality of Service such that the CAL Implementation resends CAL Messages on connections to CAL Readers that have not received the CAL Message while the CAL Message remains available for retransmit. |
| CAL-005437 | 27 | A CAL Implementation shall provide the Expiration Quality of Service such that the CAL Implementation removes expired CAL Messages from the CAL Reader's buffer. |
| CAL-005444 | 28 | A CAL Implementation shall provide the Message Buffer Quality of Service such that the CAL Implementation will buffer a configurable, maximum number of CAL Messages that the CAL Implementation has received from the CAL Client but not yet delivered to the network connection. |
| CAL-005445 | 28 | A CAL Implementation shall remove the oldest message in the CAL Writer's buffer of CAL Messages when the number of CAL Messages exceeds the value configured by the Message Buffer Quality of Service. |
| CAL-015746 | 28 | A CAL Implementation shall provide the Message Buffer Quality of Service such that the CAL Implementation will buffer a configurable, maximum number of CAL Messages that the CAL Implementation has received but not yet delivered to the CAL Client. |
| CAL-016015 | 10 | A CAL Implementation shall provide multiple CAL Clients operating in a single address space with the same behavior as if each CAL Client were executing in separate address spaces. |
| CAL-016024 | 12 | A CAL Implementation shall represent the xs:integer built-in datatype as a signed 64-bit integer. |
| CAL-016027 | 13 | A CAL Implementation shall represent the xs:duration built-in datatype as a signed 64-bit integer. |
| CAL-016028 | 13 | A CAL Implementation shall represent the xs:dateTime built-in datatype as a signed 64-bit integer. |
| CAL-016029 | 13 | A CAL Implementation shall represent the xs:time built-in datatype as a signed 64-bit integer. |
| CAL-016033 | 18 | A CAL Implementation shall support the creation of non-abstract CAL Messages and Sub-messages. |
| CAL-016035 | 18 | A CAL Implementation shall prevent the creation of abstract CAL Messages and Sub-messages. |
| CAL-016038 | 19 | A CAL Implementation shall create enumerations in an uninitialized state. |
| CAL-016043 | 22 | A CAL Writer Instance shall report an error upon invocation of the write operation when the required resources to complete the operation are not available. |
| CAL-016044 | 24 | A CAL Reader Implementation shall begin buffering received CAL Messages based on the topic QoS Settings when the topic connection is established. |
| CAL-016045 | 24 | A CAL Reader Implementation shall remove the CAL Message instance from the receive buffer once the message handler operation of each registered listener instance has been completed. |
| CAL-016046 | 24 | A CAL Implementation shall make available without modification the CAL Message accessed by the registered listener for the duration of an invoked registered listeners' message handler operation. |
| CAL-016049 | 25 | A CAL Reader Instance shall block within the read operation until the specified timeout value has expired, a new CAL Message arrives, or the Reader Instance is closed. |
| CAL-016050 | 25 | A CAL Reader Implementation shall report an error when the read operation is invoked on a Reader Instance with one or more registered listeners. |
| CAL-016052 | 25 | A CAL Reader Implementation shall remove the CAL Message instance from the receive buffer when the CAL Message is provided to the CAL Client. |
| CAL-016076 | 27 | A CAL Implementation shall provide the Reliability Quality of Service such that the CAL Implementation preserves the order of CAL Messages on a connection. |
| CAL-016079 | 28 | A CAL Implementation shall remove the oldest message in the CAL Reader's buffer of CAL Messages when number of CAL Messages exceeds the value configured by the Message Buffer Quality of Service. |
| CAL-016366 | 29 | Upon successful completion of a request to register as an Abstract Service Bus Connection Status listener, a CAL Implementation shall call the registered class' API notification method with the current state of the ASB connection. |
| CAL-016477 | 13 | A CAL Implementation shall enforce UUID conformance to RFC 4122. |
| CAL-016479 | 14 | A CAL Implementation shall generate variant 1 (Leach-Salz) UUIDs in accordance with the RFC 4122. |

---

## Summary by Section

| Spec Section | Topic | CERT Count |
|---|---|---|
| 5.1 | General / Execution Models | 3 |
| 5.2 | CAL Interface (Integer, Time, UUID) | 6 |
| 5.3 | CAL Initialization | 4 |
| 5.4 | Topics | 3 |
| 5.5 | CAL Messages (Construction, Access) | 13 |
| 5.6 | Writers | 4 |
| 5.7 | Readers (Callback + Polling) | 12 |
| 5.8 | QoS Settings | 8 |
| 5.9 | Abstract Service Bus Connection Status | 1 |
| UUID (5.2.3) | UUID conformance and generation | 3 |
| **Total** | | **55** |

---
*Note: CAL-015746 uses a 6-digit number in the 015xxx range, distinct from the 016xxx series — preserved verbatim from the source document.*
