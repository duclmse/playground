local zero = 0
local yes = true
local no = false
return (yes or 1 // zero == 0) and not (no and 1 // zero == 0)
