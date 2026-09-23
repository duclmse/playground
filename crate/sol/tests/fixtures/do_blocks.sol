function main(): i64
  local total: i64 = 0
  do
    local addend: i64 = 20
    total = total + addend
    do
      local addend: i64 = 22
      total = total + addend
    end
  end
  return total
end
