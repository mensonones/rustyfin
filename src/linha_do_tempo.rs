/// Mapeia um instante por uma razão entre intervalos, sem multiplicar em i64.
pub(crate) fn interpolar(
    instante: i64,
    origem: i64,
    destino: i64,
    intervalo_destino: i64,
    intervalo_origem: i64,
) -> Option<i64> {
    let delta = i128::from(instante) - i128::from(origem);
    let proporcao =
        (delta * i128::from(intervalo_destino)).checked_div(i128::from(intervalo_origem))?;
    i64::try_from(i128::from(destino) + proporcao).ok()
}
