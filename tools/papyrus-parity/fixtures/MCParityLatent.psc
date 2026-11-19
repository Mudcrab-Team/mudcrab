Scriptname MCParityLatent extends Quest

Bool hasRun = False

Event OnInit()
    If hasRun
        Return
    EndIf
    hasRun = True
    Debug.Trace("MCPARITY|latent|begin")
    Outer()
    Debug.Trace("MCPARITY|latent|done")
EndEvent

Function Outer()
    Debug.Trace("MCPARITY|latent|outer.before")
    Int result = Inner()
    Debug.Trace("MCPARITY|latent|outer.after=" + result)
EndFunction

Int Function Inner()
    Debug.Trace("MCPARITY|latent|inner.before")
    Utility.Wait(0.5)
    Debug.Trace("MCPARITY|latent|inner.after")
    Return 42
EndFunction
