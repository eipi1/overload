--[[
Copied from https://stackoverflow.com/a/73013693/472784
]]--

local function string_totable( str )
    local tbl = {}

    for i = 1, string.len( str ) do
        tbl[i] = string.sub( str, i, i )
    end

    return tbl
end

local function string_split(str, separator, withpattern)
    if ( separator == "" ) then return string_totable( str ) end
    if ( withpattern == nil ) then withpattern = false end

    local ret = {}
    local current_pos = 1

    for i = 1, string.len( str ) do
        local start_pos, end_pos = string.find( str, separator, current_pos, not withpattern )
        if ( not start_pos ) then break end
        ret[ i ] = string.sub( str, current_pos, start_pos - 1 )
        current_pos = end_pos + 1
    end

    ret[ #ret + 1 ] = string.sub( str, current_pos )

    return ret
end


function getValueByPath(tbl, path)
    local pathParts = string_split(path, "/")
    local enteredPaths = "/"

    local curLocation = tbl
    for i=2,#pathParts do
        local path = pathParts[i]
        if (curLocation[path] == nil) then
            path = tonumber(pathParts[i]);
            if path == nil or curLocation[path] == nil then
                print("Entry \""..pathParts[i].."\" in \""..enteredPaths.."\", does not exist.")
                return nil
            end
        end

        if (i == #pathParts) then -- If last value in pathParts
            return curLocation[path]
        else
            curLocation = curLocation[path]
            enteredPaths = enteredPaths..path.."/"
        end
    end

    return curLocation
end

return getValueByPath