Scriptname MCParityNestedBegin extends Quest

Bool hasRun = False

Event OnInit()
    If hasRun
        Return
    EndIf
    hasRun = True
    Debug.Trace("MCPARITY|nested_begin|begin")
    GotoState("Final")
    Debug.Trace("MCPARITY|nested_begin|after=" + GetState())
    Debug.Trace("MCPARITY|nested_begin|done")
EndEvent

Auto State Start
EndState

State Final
    Event OnBeginState()
        Debug.Trace("MCPARITY|nested_begin|final.enter=" + GetState())
        GotoState("Nested")
        Debug.Trace("MCPARITY|nested_begin|final.leave=" + GetState())
    EndEvent

    Event OnEndState()
        Debug.Trace("MCPARITY|nested_begin|final.end=" + GetState())
    EndEvent
EndState

State Nested
    Event OnBeginState()
        Debug.Trace("MCPARITY|nested_begin|nested.begin=" + GetState())
    EndEvent
EndState
